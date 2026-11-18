//! Input sources: node serial ports, recorded files, stdin, an ADSBee, and
//! the demo.
//! Each runs on its own thread and sends [`Event`]s down one channel, so a
//! slow or silent port never stalls the others.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::parse::{self, Line};

/// Something a source produced, stamped with the time (ms since program
/// start, or simulated time for `--demo --once`) the tracker should use.
pub enum Event {
    Line {
        node: u8,
        line: Line,
        at_ms: u64,
    },
    /// From the demo: a hub-format detection with raw messages.
    Detection {
        node: u8,
        // Boxed: a full Message Pack is ~250 bytes, much bigger than a Line.
        det: Box<link::Detection>,
        at_ms: u64,
    },
    /// From the demo: an open FPV link sighting, as a node sends the hub.
    OpenLink {
        node: u8,
        sighting: link::OpenLinkSighting,
        at_ms: u64,
    },
    /// From the demo: any other hub-format frame (DroneID, RF, DJI Wi-Fi,
    /// Wi-Fi drone, Status).
    Frame {
        node: u8,
        frame: Box<link::Frame>,
        at_ms: u64,
    },
    /// The source ended (EOF or error).
    End {
        node: u8,
        why: String,
    },
    /// From the ADSBee (or the demo's simulated one): one CSBee line, parsed.
    Adsb {
        result: Result<adsb::Report, adsb::Error>,
        at_ms: u64,
    },
    /// The ADSBee input ended.
    AdsbEnd {
        why: String,
    },
}

pub struct Clock(pub Instant);

impl Clock {
    pub fn ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
}

/// The longest line kept. A node's JSON lines are a few hundred bytes; a
/// source that never sends a newline must not grow memory without limit.
const MAX_LINE: usize = 64 * 1024;

/// What [`read_line_bounded`] found.
#[derive(Debug, PartialEq, Eq)]
enum LineRead {
    Eof,
    /// A line is in the buffer (without its newline when it had one).
    Line,
    /// The line was longer than the cap: dropped, buffer left empty.
    TooLong,
}

/// Like `read_until(b'\n')` but keeps at most `max` bytes: the rest of an
/// over-long line is read and thrown away up to its newline.
fn read_line_bounded<R: BufRead>(r: &mut R, out: &mut Vec<u8>, max: usize) -> io::Result<LineRead> {
    out.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let chunk = match r.fill_buf() {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            break;
        }
        any = true;
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            if out.len() + take > max {
                too_long = true;
                out.clear();
            } else {
                out.extend_from_slice(&chunk[..take]);
            }
        }
        r.consume(take);
        if done {
            break;
        }
    }
    Ok(match (any, too_long) {
        (false, _) => LineRead::Eof,
        (true, true) => LineRead::TooLong,
        (true, false) => LineRead::Line,
    })
}

/// Open `path` ("-" = stdin) and read JSON lines from it on a new thread.
/// With `realtime`, sleep between detections by the gaps in their `up_us`,
/// so a recording plays back at the speed it was captured.
pub fn spawn_reader(
    node: u8,
    path: String,
    realtime: bool,
    start: Instant,
    tx: Sender<Event>,
) -> io::Result<()> {
    let reader: Box<dyn Read + Send> = if path == "-" {
        Box::new(io::stdin())
    } else {
        open(Path::new(&path))?
    };
    std::thread::spawn(move || {
        let clock = Clock(start);
        let mut last_up: Option<u64> = None;
        let mut buf = BufReader::new(reader);
        let mut raw = Vec::new();
        let why = loop {
            let too_long = match read_line_bounded(&mut buf, &mut raw, MAX_LINE) {
                Ok(LineRead::Eof) => break "end of input".to_string(),
                Ok(LineRead::Line) => false,
                Ok(LineRead::TooLong) => true,
                Err(e) => break e.to_string(),
            };
            // A node that panics may print non-UTF-8; keep going.
            let text = String::from_utf8_lossy(&raw);
            // An over-long line counts as unparsed.
            let line = if too_long {
                Line::Bad
            } else {
                parse::line(&text)
            };
            if realtime
                && let Line::Rid(r) = &line
                && let Some(up) = r.up_us
            {
                if let Some(prev) = last_up.filter(|p| up > *p) {
                    // Cap the gap so a recording with a reboot in it does not hang.
                    std::thread::sleep(Duration::from_micros((up - prev).min(5_000_000)));
                }
                last_up = Some(up);
            }
            if tx
                .send(Event::Line {
                    node,
                    line,
                    at_ms: clock.ms(),
                })
                .is_err()
            {
                return;
            }
        };
        let _ = tx.send(Event::End { node, why });
    });
    Ok(())
}

/// Read an ADSBee's CSBee output (`--adsbee`): its USB console or a
/// USB-UART on its COMMS_UART pins, or a recorded file of the same text.
/// With `realtime`, a recording plays back at one report per second (the
/// ADSBee ends each once-a-second report with its `#S:` line).
pub fn spawn_adsbee(
    path: String,
    realtime: bool,
    start: Instant,
    tx: Sender<Event>,
) -> io::Result<()> {
    let mut reader = open(Path::new(&path))?;
    std::thread::spawn(move || {
        let clock = Clock(start);
        let mut split = adsb::LineSplitter::new();
        let mut buf = [0u8; 512];
        let why = loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break "end of input".to_string(),
                Ok(n) => n,
                Err(e) => break e.to_string(),
            };
            let mut gone = false;
            split.feed(&buf[..n], |result| {
                if gone {
                    return;
                }
                let end_of_report = matches!(result, Ok(adsb::Report::Stats(_)));
                gone = tx
                    .send(Event::Adsb {
                        result,
                        at_ms: clock.ms(),
                    })
                    .is_err();
                if realtime && end_of_report {
                    std::thread::sleep(Duration::from_secs(1));
                }
            });
            if gone {
                return;
            }
        };
        let _ = tx.send(Event::AdsbEnd { why });
    });
    Ok(())
}

/// Open a file, or a serial device in raw mode.
fn open(path: &Path) -> io::Result<Box<dyn Read + Send>> {
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    let is_tty = std::fs::metadata(path)?.file_type().is_char_device();
    let file = std::fs::OpenOptions::new()
        .read(true)
        // Never let the node's port become our controlling terminal.
        .custom_flags(if is_tty { libc::O_NOCTTY } else { 0 })
        .open(path)?;
    if is_tty {
        raw_mode(&file)?;
    }
    Ok(Box::new(file))
}

/// Raw mode for a serial port: no echo (a tty in its default mode echoes
/// what it receives straight back out, into the node, and the bench
/// transmitter treats incoming bytes as commands), no line editing, no
/// CR/LF translation. 115200 baud for USB-UART bridges; USB CDC ignores it.
fn raw_mode(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: plain termios calls on a file descriptor we own; `t` is fully
    // initialised by tcgetattr before use.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return Err(io::Error::last_os_error());
        }
        libc::cfmakeraw(&mut t);
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        libc::cfsetispeed(&mut t, libc::B115200);
        libc::cfsetospeed(&mut t, libc::B115200);
        if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_without_a_newline_cannot_grow_memory() {
        // 1 MiB with no newline, then a normal line. `read_until` would hold
        // all of the first in the buffer.
        let mut data = vec![b'x'; 1 << 20];
        data.extend_from_slice(b"\nnext\n");
        let mut r = BufReader::with_capacity(4096, &data[..]);
        let mut raw = Vec::new();
        assert_eq!(
            read_line_bounded(&mut r, &mut raw, MAX_LINE).unwrap(),
            LineRead::TooLong
        );
        assert!(raw.is_empty());
        assert_eq!(
            read_line_bounded(&mut r, &mut raw, MAX_LINE).unwrap(),
            LineRead::Line
        );
        assert_eq!(raw, b"next\n");
        assert_eq!(
            read_line_bounded(&mut r, &mut raw, MAX_LINE).unwrap(),
            LineRead::Eof
        );
    }

    #[test]
    fn the_cap_is_per_line_and_inclusive() {
        let mut data = vec![b'a'; MAX_LINE - 1];
        data.push(b'\n');
        data.extend_from_slice(&vec![b'b'; MAX_LINE]);
        data.push(b'\n');
        let mut r = BufReader::with_capacity(512, &data[..]);
        let mut raw = Vec::new();
        assert_eq!(
            read_line_bounded(&mut r, &mut raw, MAX_LINE).unwrap(),
            LineRead::Line
        );
        assert_eq!(raw.len(), MAX_LINE);
        // MAX_LINE bytes plus the newline is one over.
        assert_eq!(
            read_line_bounded(&mut r, &mut raw, MAX_LINE).unwrap(),
            LineRead::TooLong
        );
        // A last line with no newline at all is still a line.
        let mut r = &b"tail"[..];
        assert_eq!(
            read_line_bounded(&mut r, &mut raw, MAX_LINE).unwrap(),
            LineRead::Line
        );
        assert_eq!(raw, b"tail");
    }
}
