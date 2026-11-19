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
//!   --key <hex>          the unit's public key, confirmed OUTSIDE the log
//!                        (65-byte uncompressed SEC1, 130 hex digits);
//!                        checkpoints signed by any other key fail
//!   --keys <file>        trusted keys, one per line: 130 hex digits, then a
//!                        label (free text); `#` starts a comment
//!   --allow-gaps         missing sequence numbers or files are gaps, not
//!                        failures (for records collected from several
//!                        places); a gap may hide removed records
//!   --allow-test-key     accept the published test key (this project's
//!                        own tests only: anyone can sign with it)
//!   --quiet              verdict lines only
//!
//! exit codes:
//!   0  VERIFIED: every link and signature checks out against a key given
//!      with --key/--keys, and a signature covers every record checked
//!   1  FAILED: the log is not what its signer wrote, or it is signed with
//!      the published test key, or by a key not supplied (details name the
//!      file, byte offset, chain and record number)
//!   2  could not run (bad arguments, unreadable input)
//!   3  SIGNER CHECKED, WITH GAPS: nothing contradicts the log and the
//!      signer is a supplied key, but some records are not covered by a
//!      signature or (with --allow-gaps) are missing
//!   4  SELF-CONSISTENT, SIGNER NOT CHECKED: no key was supplied, so the
//!      log was checked only against the key it names itself; anyone could
//!      have made it
//!
//! No verdict proves the log is complete: records after the last one, newer
//! files, a reformatted card or a boot never uploaded leave no trace.
//! ```
//!
//! What it checks and what it cannot: docs/EVIDENCE.md. The record layout,
//! to check a log with other tools: docs/EVIDENCE-FORMAT.md.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use evidence::format;
use evidence::verify::{self, hex, Input, Level, Options, Report};

fn usage() -> ExitCode {
    eprintln!("usage: evidence-verify [--list] [--key HEX | --keys FILE] [--allow-gaps] [--allow-test-key] [--quiet] <EVIDENCE-dir | EVnnnnnn.EVL... | records.bin | bundle.json>");
    eprintln!("  --key HEX    the unit's public key (130 hex digits), confirmed outside the log:");
    eprintln!("               from the unit's screen or console at setup, or your own key record");
    eprintln!("  --keys FILE  trusted keys, one per line: HEX then a label; # comments");
    eprintln!("  Without --key or --keys the log is checked only against the key it names itself,");
    eprintln!("  which anyone could have generated: the result is never VERIFIED.");
    eprintln!("exit: 0 verified, 1 failed, 2 could not run, 3 signer checked but gaps or unsigned records,");
    eprintln!("      4 self-consistent but signer not checked (no key supplied)");
    ExitCode::from(2)
}

fn parse_key(h: &str) -> Option<[u8; 65]> {
    verify::unhex(h).and_then(|v| <[u8; 65]>::try_from(v).ok())
}

/// A trusted-keys file: `HEX label...` per line, `#` comments, blank lines.
fn load_keys(path: &Path) -> Result<Vec<([u8; 65], String)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (h, label) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let Some(k) = parse_key(h) else {
            return Err(format!("{} line {}: not a 130-hex-digit public key", path.display(), i + 1));
        };
        let label = label.trim();
        let label = if label.is_empty() { format!("{} line {}", path.display(), i + 1) } else { label.to_string() };
        out.push((k, label));
    }
    if out.is_empty() {
        return Err(format!("{}: no keys", path.display()));
    }
    Ok(out)
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
            "--allow-test-key" => opts.allow_test_key = true,
            "--key" => {
                let Some(k) = args.next().and_then(|h| parse_key(&h)) else {
                    eprintln!("--key needs 130 hex digits");
                    return usage();
                };
                if opts.expect_key.is_none() {
                    opts.expect_key = Some(k);
                } else {
                    opts.trusted_keys.push((k, "the key given with --key".into()));
                }
            }
            "--keys" => {
                let Some(f) = args.next() else {
                    eprintln!("--keys needs a file");
                    return usage();
                };
                match load_keys(Path::new(&f)) {
                    Ok(k) => opts.trusted_keys.extend(k),
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::from(2);
                    }
                }
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
        verify::verify_bundle(&bytes, &opts)
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
    println!("{}", report.verdict());
    println!("{}", report.not_proven());
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
                "  key {} (log claims: {})",
                std::str::from_utf8(&evidence::fingerprint(&k)).unwrap_or("?"),
                class.map_or("unknown class", |c| c.note())
            );
            println!("  public key {}", hex(&k));
        }
        match &c.trusted_as {
            Some(label) => println!("  signer: checked against {label}"),
            None => println!("  signer: NOT CHECKED (no matching key supplied from outside the log)"),
        }
        if let Some(f) = c.fw_hash {
            println!("  firmware SHA-256 {}{}", hex(&f), if f == [0; 32] { " (not filled in: flashed without espflash)" } else { "" });
        }
        if let Some(b) = c.build {
            let state = match b.git_state {
                format::boot::GIT_CLEAN => "",
                format::boot::GIT_DIRTY => " WITH CHANGES",
                _ => " (tree state unknown)",
            };
            println!(
                "  built {} from source commit {}{state}",
                verify::fmt_utc(b.built_unix_s.saturating_mul(1000)),
                if b.git == [0; 20] { "unknown".to_string() } else { hex(&b.git) }
            );
        }
        match (c.utc_first, c.utc_last) {
            (Some(a), Some(b)) => println!("  UTC {} .. {}", verify::fmt_utc(a), verify::fmt_utc(b)),
            _ => println!("  UTC: none (no GPS time in any record)"),
        }
        println!(
            "  time: {} records without UTC ({} no GPS time yet, {} GPS time untrusted), {} free-running; position: {} without, {} typed in",
            c.no_time, c.no_time - c.untrusted_time, c.untrusted_time, c.free_running, c.no_position, c.manual_position
        );
        if let Some(k) = c.continuity {
            println!(
                "  boot counter {} ({}); previous boot {} with {} records stored{}",
                k.counter,
                evidence::continuity::Continuity::status_name(k.status),
                if k.prev_boot == [0; 8] { "-".to_string() } else { hex(&k.prev_boot) },
                k.prev_records,
                c.vouched_by_next.map_or(String::new(), |l| format!("; records to #{l} vouched for by the next boot"))
            );
        }
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
