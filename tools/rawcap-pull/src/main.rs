//! Rebuild node-sdr captures from a log of its USB serial port.
//!
//! ```text
//! rawcap-pull [<log>|-] [--out <dir>]
//! ```
//!
//! Reads the log (a file, or stdin with `-` or no argument), finds every
//! `@H`/`@D`/`@E` dump in it (other lines are ignored, any order, repeats
//! welcome), and for each complete one writes, in `--out` (default `.`):
//!
//! - `c5-<id>.<kind>` — the raw payload exactly as captured (`nib8`, `w32`
//!   or `c16`; see `rawcap::dump`);
//! - `c5-<id>.cs16` — the same samples as interleaved little-endian `i16`
//!   I, Q at the capture's own rate;
//! - `c5-<id>.json` — the header (rate, frequency, gain, window...).
//!
//! and prints the `droneid-file` command for it. For an incomplete dump it
//! prints the `resend` commands to type on the node's console; append the
//! new output to the same log and run again.
//!
//! It never opens a serial port itself. Capture with, e.g.,
//! `tools/dev-board.sh --log 60 c5 | tee c5.log`, typing commands in another
//! terminal, or any serial terminal that logs to a file.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use rawcap::dump::{Collector, to_ci16};

fn main() -> ExitCode {
    let mut input: Option<String> = None;
    let mut out = PathBuf::from(".");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => match args.next() {
                Some(d) => out = PathBuf::from(d),
                None => return usage(),
            },
            "-h" | "--help" => return usage(),
            _ if input.is_none() => input = Some(a),
            _ => return usage(),
        }
    }
    let reader: Box<dyn Read> = match input.as_deref() {
        None | Some("-") => Box::new(std::io::stdin()),
        Some(p) => match std::fs::File::open(p) {
            Ok(f) => Box::new(f),
            Err(e) => {
                eprintln!("{p}: {e}");
                return ExitCode::from(1);
            }
        },
    };
    let mut c = Collector::default();
    // Serial logs can carry invalid UTF-8 (a reset mid-byte): read bytes.
    for line in BufReader::new(reader).split(b'\n') {
        let Ok(line) = line else { break };
        c.feed(&String::from_utf8_lossy(&line));
    }
    if c.dumps.is_empty() {
        eprintln!("no dumps found (looking for lines starting with @H / @D / @E)");
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("{}: {e}", out.display());
        return ExitCode::from(1);
    }
    let mut failed = false;
    for (id, p) in &c.dumps {
        let m = &p.meta;
        match p.payload() {
            Ok(data) => {
                let base = out.join(format!("c5-{id}"));
                let raw = base.with_extension(m.kind.name());
                let cs16 = base.with_extension("cs16");
                let json = base.with_extension("json");
                let w = std::fs::write(&raw, &data)
                    .and_then(|_| std::fs::write(&cs16, to_ci16(m, &data)))
                    .and_then(|_| std::fs::write(&json, format!("{m}\n")));
                if let Err(e) = w {
                    eprintln!("dump {id}: writing: {e}");
                    failed = true;
                    continue;
                }
                println!(
                    "dump {id}: {} {} bytes, {:.3} ms at {} MS/s, LO {:.3} MHz, gain {}, lsb {} -> {}",
                    m.kind.name(),
                    m.bytes,
                    m.bytes as f64 / m.kind.sample_bytes() as f64 / m.rate as f64 * 1e3,
                    m.rate as f64 / 1e6,
                    m.freq_khz as f64 / 1e3,
                    m.gain,
                    m.lsb,
                    cs16.display()
                );
                println!("  droneid-file {} --rate {} --format ci16 --redact --failures", cs16.display(), m.rate);
            }
            Err(e) => {
                failed = true;
                let bad = if p.bad.is_empty() { String::new() } else { format!(" ({} line(s) failed their CRC)", p.bad.len()) };
                println!("dump {id}: incomplete{bad}: {e}");
            }
        }
    }
    if c.orphans > 0 {
        println!("{} data line(s) belonged to dumps whose @H header is not in this log", c.orphans);
    }
    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn usage() -> ExitCode {
    eprintln!("usage: rawcap-pull [<log>|-] [--out <dir>]");
    ExitCode::from(2)
}
