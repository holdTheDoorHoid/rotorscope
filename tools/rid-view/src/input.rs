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
            raw.clear();
            match buf.read_until(b'\n', &mut raw) {
                Ok(0) => break "end of input".to_string(),
                Ok(_) => {}
                Err(e) => break e.to_string(),
            }
            // A node that panics may print non-UTF-8; keep going.
            let text = String::from_utf8_lossy(&raw);
            let line = parse::line(&text);
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
