//! Shared helpers for the text formatters: a `fmt::Write` over a fixed
//! buffer, UTC timestamps, and the bits of escaping XML and CSV need.

use core::fmt::{self, Write};

/// Why a formatter could not produce output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// The caller's buffer is too small. Nothing useful was written.
    BufferTooSmall,
    /// The output needs a position the track does not have (CoT of a drone
    /// with no Location, or of an operator with no System position).
    NoPosition,
    /// The output needs UTC and the hub has none yet (no GPS time).
    NoTime,
}

/// `fmt::Write` into a byte slice; remembers overflow instead of panicking.
pub(crate) struct Buf<'a> {
    buf: &'a mut [u8],
    len: usize,
    overflow: bool,
}

impl<'a> Buf<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            len: 0,
            overflow: false,
        }
    }

    pub fn finish(self) -> Result<usize, FormatError> {
        if self.overflow {
            Err(FormatError::BufferTooSmall)
        } else {
            Ok(self.len)
        }
    }
}

impl Write for Buf<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        match self.buf.get_mut(self.len..end) {
            Some(dst) => {
                dst.copy_from_slice(s.as_bytes());
                self.len = end;
                Ok(())
            }
            None => {
                self.overflow = true;
                Err(fmt::Error)
            }
        }
    }
}

/// `2026-11-09T14:03:07.250Z` for Unix milliseconds.
pub(crate) struct IsoTime(pub u64);

impl fmt::Display for IsoTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = self.0 % 1000;
        let secs = self.0 / 1000;
        let (days, sod) = (secs / 86_400, secs % 86_400);
        let (y, m, d) = civil_from_days(days as i64);
        write!(
            f,
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
            sod / 3600,
            sod / 60 % 60,
            sod % 60
        )
    }
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
/// Howard Hinnant's `civil_from_days` (public domain algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Text with XML's five special characters escaped. Control characters
/// (not allowed in XML 1.0) become `?`.
pub(crate) struct Xml<'a>(pub &'a str);

impl fmt::Display for Xml<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            match c {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                '"' => f.write_str("&quot;")?,
                '\'' => f.write_str("&apos;")?,
                c if (c as u32) < 0x20 => f.write_char('?')?,
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}

/// A CSV field: quoted (RFC 4180) only when it contains a comma, quote or
/// line break, so ordinary IDs stay readable.
pub(crate) struct CsvField<'a>(pub &'a str);

impl fmt::Display for CsvField<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.0.contains([',', '"', '\n', '\r']) {
            return f.write_str(self.0);
        }
        f.write_char('"')?;
        for c in self.0.chars() {
            if c == '"' {
                f.write_str("\"\"")?;
            } else {
                f.write_char(c)?;
            }
        }
        f.write_char('"')
    }
}

/// A fixed text field as display text: the string when it is clean ASCII,
/// else lowercase hex (binary UTM UUIDs and session IDs), matching how the
/// `rid` crate serializes it.
pub(crate) struct IdText<'a, const N: usize>(pub &'a rid::message::FixedStr<N>);

impl<const N: usize> fmt::Display for IdText<'_, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.as_str() {
            Some(s) if self.0.is_clean_text() => f.write_str(s),
            _ => self
                .0
                .as_bytes()
                .iter()
                .try_for_each(|b| write!(f, "{b:02x}")),
        }
    }
}

/// Render a `Display` into a stack buffer, for passing to escapers that
/// take `&str`. Truncates (on a char boundary) if it does not fit.
pub(crate) fn render<const N: usize>(v: impl fmt::Display) -> heapless::String<N> {
    struct Trunc<'a, const N: usize>(&'a mut heapless::String<N>);
    impl<const N: usize> Write for Trunc<'_, N> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            for c in s.chars() {
                if self.0.push(c).is_err() {
                    return Err(fmt::Error);
                }
            }
            Ok(())
        }
    }
    let mut out = heapless::String::new();
    let _ = write!(Trunc(&mut out), "{v}");
    out
}

/// `aa:bb:cc:dd:ee:ff`
pub(crate) struct Mac<'a>(pub &'a [u8; 6]);

impl fmt::Display for Mac<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    }
}

impl serde::Serialize for Mac<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_time() {
        let s: heapless::String<32> = render(IsoTime(0));
        assert_eq!(s, "1970-01-01T00:00:00.000Z");
        // 2026-11-09T14:03:07.250Z
        let s: heapless::String<32> = render(IsoTime(1_794_232_987_250));
        assert_eq!(s, "2026-11-09T14:03:07.250Z");
        // Leap day.
        let s: heapless::String<32> = render(IsoTime(1_709_164_800_000));
        assert_eq!(s, "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn escaping() {
        let s: heapless::String<64> = render(Xml("a<b>&\"c'\x01"));
        assert_eq!(s, "a&lt;b&gt;&amp;&quot;c&apos;?");
        let s: heapless::String<64> = render(CsvField("plain"));
        assert_eq!(s, "plain");
        let s: heapless::String<64> = render(CsvField("a,\"b\""));
        assert_eq!(s, "\"a,\"\"b\"\"\"");
    }

    #[test]
    fn buf_overflow_is_reported() {
        let mut b = [0u8; 4];
        let mut w = Buf::new(&mut b);
        assert!(w.write_str("12345").is_err());
        assert_eq!(w.finish(), Err(FormatError::BufferTooSmall));
    }
}
