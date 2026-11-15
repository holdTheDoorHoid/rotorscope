//! evidence-verify: check a Rotorscope evidence log or export bundle.
//!
//! ```text
//! evidence-verify [options] <input>...
//!
//! inputs:
//!   EVIDENCE/            a card's evidence folder: every EVnnnnnn.EVL in it
//!   EV000003.EVL ...     card files (checked in file-number order)
//!   batch.bin            any file of records back to back
//!   bundle.json          an export bundle from evidence-server
//!
//! options:
//!   --list               print every record, one line each
//!   --key <hex>          only accept checkpoints signed by this public key
//!                        (65-byte uncompressed SEC1, 130 hex digits)
//!   --allow-gaps         missing sequence numbers are gaps, not failures
//!                        (for records collected from several places)
//!   --quiet              verdict line only
//!
//! exit codes:
//!   0  verified: every link and signature checks out and a signature
//!      covers every record
//!   1  FAILED: the log is not what the unit wrote (details name the file,
//!      byte offset, chain and record number)
//!   2  could not run (bad arguments, unreadable input)
//!   3  nothing contradicts the log, but some records are not covered by a
//!      signature or some are missing (gaps listed)
//! ```
//!
//! What it checks and what it cannot: docs/EVIDENCE.md. The record layout,
//! to check a log with other tools: docs/EVIDENCE-FORMAT.md.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use evidence::format;
use evidence::verify::{self, hex, Input, Level, Options, Report};

fn usage() -> ExitCode {
    eprintln!("usage: evidence-verify [--list] [--key HEX] [--allow-gaps] [--quiet] <EVIDENCE-dir | EVnnnnnn.EVL... | records.bin | bundle.json>");
    eprintln!("exit: 0 verified, 1 failed, 2 could not run, 3 gaps or unsigned records");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut list = false;
    let mut quiet = false;
    let mut opts = Options::default();
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--list" => list = true,
            "--quiet" => quiet = true,
            "--allow-gaps" => opts.gaps_allowed = true,
            "--key" => {
                let Some(k) = args.next().and_then(|h| verify::unhex(&h)).and_then(|v| <[u8; 65]>::try_from(v).ok()) else {
                    eprintln!("--key needs 130 hex digits");
                    return usage();
                };
                opts.expect_key = Some(k);
            }
            "-h" | "--help" => {
                usage();
                return ExitCode::SUCCESS;
            }
            s if s.starts_with('-') => return usage(),
            _ => paths.push(a.into()),
        }
    }
    if paths.is_empty() {
        return usage();
    }

    let report = if paths.len() == 1 && paths[0].extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
        let bytes = match std::fs::read(&paths[0]) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{}: {e}", paths[0].display());
                return ExitCode::from(2);
            }
        };
        let sha = hex(&evidence::sha256(&bytes));
        if !quiet {
            println!("bundle {}  SHA-256 {sha}", paths[0].display());
            println!("  (compare with the export entry in the server's audit log and the custody record)");
        }
        verify::verify_bundle(&bytes)
    } else {
        let inputs = match load(&paths) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        };
        if !quiet {
            for i in &inputs {
                println!("input {}  {} bytes  SHA-256 {}", i.name, i.bytes.len(), hex(&evidence::sha256(&i.bytes)));
            }
        }
        verify::verify(&inputs, &opts)
    };
    if !quiet {
        print_report(&report, list);
    }
    let code = report.exit_code();
    println!(
        "{}",
        match code {
            0 => "VERIFIED: every record links, every signature checks, a signature covers every record.",
            1 => "FAILED: the log is not what the unit wrote. See the FAIL lines above.",
            _ => "VERIFIED WITH GAPS: nothing contradicts the log, but not every record is covered (see GAP lines).",
        }
    );
    ExitCode::from(code as u8)
}

/// Directories expand to their EVnnnnnn.EVL files; card files are sorted by
/// number; other files keep the order given.
fn load(paths: &[PathBuf]) -> Result<Vec<Input>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            let rd = std::fs::read_dir(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let mut found: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| verify::file_number(&f.to_string_lossy()).is_some())
                .collect();
            if found.is_empty() {
                return Err(format!("{}: no EVnnnnnn.EVL files", p.display()));
            }
            found.sort();
            files.extend(found);
        } else {
            files.push(p.clone());
        }
    }
    if files.iter().all(|f| verify::file_number(&f.to_string_lossy()).is_some()) {
        files.sort_by_key(|f| verify::file_number(&f.to_string_lossy()));
    }
    files
        .iter()
        .map(|f| {
            let bytes = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
            Ok(Input::new(name_of(f), bytes))
        })
        .collect()
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn print_report(r: &Report, list: bool) {
    if list {
        for (_, _, rec) in &r.records {
            println!("  {}", verify::describe(&rec.parse()));
        }
    }
    for c in &r.chains {
        println!("chain (boot) {}", hex(&c.boot));
        println!(
            "  records {}..{}: {} ({} detections, {} events, {} checkpoints); signed through #{}",
            c.first_seq.unwrap_or(0),
            c.last_seq.unwrap_or(0),
            c.records,
            c.detections,
            c.events,
            c.checkpoints,
            c.signed_through.map_or("-".to_string(), |s| s.to_string())
        );
        if let Some(k) = c.key {
            let class = c.key_class.and_then(evidence::KeyClass::from_u8);
            println!(
                "  key {} ({})",
                std::str::from_utf8(&evidence::fingerprint(&k)).unwrap_or("?"),
                class.map_or("unknown class", |c| c.note())
            );
            println!("  public key {}", hex(&k));
        }
        if let Some(f) = c.fw_hash {
            println!("  firmware SHA-256 {}{}", hex(&f), if f == [0; 32] { " (not filled in: flashed without espflash)" } else { "" });
        }
        match (c.utc_first, c.utc_last) {
            (Some(a), Some(b)) => println!("  UTC {} .. {}", verify::fmt_utc(a), verify::fmt_utc(b)),
            _ => println!("  UTC: none (no GPS time in any record)"),
        }
        println!(
            "  time: {} records without UTC, {} free-running; position: {} without, {} typed in",
            c.no_time, c.free_running, c.no_position, c.manual_position
        );
        for (a, b) in &c.missing {
            println!("  missing records #{a}..#{b}");
        }
    }
    let mut findings = r.findings.clone();
    findings.sort_by_key(|f| std::cmp::Reverse(f.level));
    for f in &findings {
        let at = f.at.to_string();
        if at.is_empty() {
            println!("{:4}  {}", f.level.name(), f.what);
        } else {
            println!("{:4}  {}: {}", f.level.name(), at, f.what);
        }
    }
    let fails = findings.iter().filter(|f| f.level == Level::Fail).count();
    let gaps = findings.iter().filter(|f| f.level == Level::Gap).count();
    println!("{} records checked; {fails} failures, {gaps} gaps", r.records.len());
    let _ = format::MAGIC;
}
