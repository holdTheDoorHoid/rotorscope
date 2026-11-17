//! Build CSBee lines exactly as the ADSBee prints them (feature `encode`):
//! test fixtures, and the simulated aircraft of the laptop viewer's and the
//! hub's demo modes. The field formats mirror the `snprintf` format strings
//! in the ADSBee's `csbee_utils.hh` (see the crate docs).

use core::fmt::Write;

use crate::{Aircraft, MAX_LINE, Source, Stats, crc16};

/// `fmt::Write` into a fixed buffer.
struct Buf<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl Write for Buf<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        let dst = self.buf.get_mut(self.len..end).ok_or(core::fmt::Error)?;
        dst.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Append `CRC\r\n` to the body in `buf[..len]`; returns the line length.
fn seal(mut w: Buf<'_>) -> Option<usize> {
    let crc = crc16(&w.buf[..w.len]);
    write!(w, "{crc:X}\r\n").ok()?;
    (w.len <= MAX_LINE).then_some(w.len)
}

/// Write `a` as a `#A:` / `#U:` line with its CRC and `\r\n`. Returns the
/// length, or `None` if `buf` is too small (a line is at most
/// [`MAX_LINE`] bytes).
pub fn aircraft(a: &Aircraft, buf: &mut [u8]) -> Option<usize> {
    let mut w = Buf { buf, len: 0 };
    let tag = char::from(a.source.tag());
    let call = if a.callsign.is_empty() {
        "?"
    } else {
        a.callsign.as_str()
    };
    write!(w, "#{tag}:{:06X},{:X},{call},", a.icao, a.flags).ok()?;
    match a.squawk {
        Some(s) => write!(w, "{:04o},", s & 0o7777).ok()?,
        None => w.write_str("?,").ok()?,
    }
    write!(
        w,
        "{},{:.5},{:.5},{},{},{:.0},{},{},{},",
        a.category,
        a.lat_deg,
        a.lon_deg,
        a.baro_alt_ft,
        a.gnss_alt_ft,
        a.direction_deg,
        a.speed_kt,
        a.baro_vrate_fpm,
        a.gnss_vrate_fpm
    )
    .ok()?;
    if a.source != Source::ModeS {
        write!(w, "{},", a.uat_emergency).ok()?;
    }
    write!(
        w,
        "{:X},{:X},{},{},{},{},",
        a.nicnac, a.acdims, a.version, a.signal_dbm, a.signal_quality, a.frames_per_s
    )
    .ok()?;
    if a.source == Source::ModeS {
        write!(w, "{},", a.es_frames_per_s).ok()?;
    }
    seal(w)
}

/// Write `s` as a `#S:` line with its CRC and `\r\n`.
pub fn stats(s: &Stats, buf: &mut [u8]) -> Option<usize> {
    let mut w = Buf { buf, len: 0 };
    write!(
        w,
        "#S:{},{},{},{},{},{},{},{},{},{},{},",
        s.version,
        s.demods_per_s,
        s.raw_squitters_per_s,
        s.squitters_per_s,
        s.raw_extended_squitters_per_s,
        s.extended_squitters_per_s,
        s.raw_uat_per_s,
        s.uat_per_s,
        s.aircraft,
        s.tscal,
        s.uptime_s
    )
    .ok()?;
    seal(w)
}
