//! Rebuild node-sdr captures from a log of its USB serial port.
//!
//! ```text
//! rawcap-pull [<log>|-] [--out <dir>] [--spectrum]
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
//! `--spectrum` also prints each capture's averaged power spectrum in 40
//! bins with a bar chart: the bring-up checks (filter width, DC spike, which
//! side of the LO a known signal lands on, aliasing) read off it.
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
    let mut spectrum = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => match args.next() {
                Some(d) => out = PathBuf::from(d),
                None => return usage(),
            },
            "--spectrum" => spectrum = true,
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
                if spectrum {
                    print_spectrum(&to_ci16(m, &data), m.rate);
                }
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

/// Averaged 1024-point power spectrum of ci16 samples, shown as 40 bins
/// from -fs/2 to +fs/2 (dB relative to the strongest bin).
fn print_spectrum(ci16: &[u8], rate: u32) {
    const N: usize = 1024;
    let x: Vec<(f64, f64)> = ci16
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| (i16::from_le_bytes([b[0], b[1]]) as f64, i16::from_le_bytes([b[2], b[3]]) as f64))
        .collect();
    if x.len() < N {
        return;
    }
    let mut acc = vec![0f64; N];
    let mut frames = 0;
    for f in x.as_chunks::<N>().0 {
        // Hann window, then FFT.
        let mut b: Vec<(f64, f64)> = f
            .iter()
            .enumerate()
            .map(|(n, &(i, q))| {
                let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / N as f64).cos();
                (i * w, q * w)
            })
            .collect();
        fft(&mut b);
        for (a, v) in acc.iter_mut().zip(&b) {
            *a += v.0 * v.0 + v.1 * v.1;
        }
        frames += 1;
    }
    // Reorder to -fs/2..fs/2 and group into 40 bins.
    let shifted: Vec<f64> = (0..N).map(|k| acc[(k + N / 2) % N] / frames as f64).collect();
    let bins = 40;
    let per = N as f64 / bins as f64;
    let p: Vec<f64> = (0..bins)
        .map(|j| {
            let (a, b) = ((j as f64 * per) as usize, ((j + 1) as f64 * per) as usize);
            shifted[a..b].iter().sum::<f64>() / (b - a) as f64
        })
        .collect();
    let top = p.iter().cloned().fold(1e-30, f64::max);
    println!("  spectrum ({frames} x {N}-pt, Hann), dB re strongest bin:");
    for (j, v) in p.iter().enumerate() {
        let f = (-0.5 + (j as f64 + 0.5) / bins as f64) * rate as f64 / 1e6;
        let db = 10.0 * (v / top).log10();
        let bar = ((db + 60.0).max(0.0) / 1.5) as usize;
        println!("  {f:+7.2} MHz {db:6.1} {}", "#".repeat(bar));
    }
}

/// In-place radix-2 FFT (N a power of two).
fn fft(a: &mut [(f64, f64)]) {
    let n = a.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        for s in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (sn, cs) = (ang * k as f64).sin_cos();
                let u = a[s + k];
                let v = a[s + k + len / 2];
                let t = (v.0 * cs - v.1 * sn, v.0 * sn + v.1 * cs);
                a[s + k] = (u.0 + t.0, u.1 + t.1);
                a[s + k + len / 2] = (u.0 - t.0, u.1 - t.1);
            }
        }
        len <<= 1;
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: rawcap-pull [<log>|-] [--out <dir>] [--spectrum]");
    ExitCode::from(2)
}
