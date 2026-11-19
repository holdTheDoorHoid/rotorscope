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
//!   --check-timestamps DIR   (bundles) write each RFC 3161 token, and the
//!                        chain and revocation list stored with it, to DIR
//!                        and check its signature with the system's
//!                        `openssl ts -verify` at the token's own time; the
//!                        command is printed, so it can be run elsewhere when
//!                        openssl is not installed here
//!   --tsa-ca FILE        a time-stamp authority's root certificate (PEM)
//!                        obtained OUTSIDE the bundle, e.g. from its website;
//!                        repeat for each authority. Without it the check
//!                        uses the chain the server stored, which proves
//!                        less (the server supplied it)
//!   --btc-header H:HEX   (bundles) the 80-byte header of Bitcoin block H, hex,
//!                        to check OpenTimestamps proofs against; repeat
//!   --btc-headers FILE   the same, one `H HEX` per line, `#` comments.
//!                        Headers are never fetched: get them from your own
//!                        Bitcoin node (bitcoin-cli getblockheader
//!                        $(bitcoin-cli getblockhash H) false) or from
//!                        several sources you trust. A pending proof is
//!                        reported as not yet anchored; with
//!                        --check-timestamps each proof is also written out
//!                        as DIR/stamp-N.ots for other OpenTimestamps tools
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

#[cfg(test)]
mod fuzz_tests;

fn usage() -> ExitCode {
    eprintln!("usage: evidence-verify [--list] [--key HEX | --keys FILE] [--allow-gaps] [--allow-test-key] [--quiet] <EVIDENCE-dir | EVnnnnnn.EVL... | records.bin | bundle.json>");
    eprintln!("  --key HEX    the unit's public key (130 hex digits), confirmed outside the log:");
    eprintln!("               from the unit's screen or console at setup, or your own key record");
    eprintln!("  --keys FILE  trusted keys, one per line: HEX then a label; # comments");
    eprintln!("  --check-timestamps DIR [--tsa-ca ROOT.pem]...  check a bundle's time-stamp tokens with openssl");
    eprintln!("  --btc-header H:HEX | --btc-headers FILE   Bitcoin block headers (you supply them) for OpenTimestamps proofs");
    eprintln!("  Without --key or --keys the log is checked only against the key it names itself,");
    eprintln!("  which anyone could have generated: the result is never VERIFIED.");
    eprintln!("exit: 0 verified, 1 failed, 2 could not run, 3 signer checked but gaps or unsigned records,");
    eprintln!("      4 self-consistent but signer not checked (no key supplied)");
    ExitCode::from(2)
}

/// `HEIGHT:HEX` (or `HEIGHT HEX`).
fn parse_header_arg(s: &str) -> Option<(u64, evidence::ots::BlockHeader)> {
    let (h, x) = s.split_once([':', ' '])?;
    Some((h.trim().parse().ok()?, evidence::ots::BlockHeader::from_hex(x)?))
}

/// A headers file: `HEIGHT HEX` per line, `#` comments, blank lines.
fn load_headers(path: &Path) -> Result<Vec<(u64, evidence::ots::BlockHeader)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some(h) = line.split_once(char::is_whitespace).and_then(|(h, x)| parse_header_arg(&format!("{h}:{}", x.trim()))) else {
            return Err(format!("{} line {}: not HEIGHT and an 80-byte header in hex (160 digits)", path.display(), i + 1));
        };
        out.push(h);
    }
    if out.is_empty() {
        return Err(format!("{}: no headers", path.display()));
    }
    Ok(out)
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
    let mut stamp_dir: Option<PathBuf> = None;
    let mut tsa_cas: Vec<PathBuf> = Vec::new();
    let mut headers_given = false;
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
            "--check-timestamps" => match args.next() {
                Some(d) => stamp_dir = Some(d.into()),
                None => {
                    eprintln!("--check-timestamps needs a folder to write the tokens to");
                    return usage();
                }
            },
            "--tsa-ca" => match args.next() {
                Some(f) => tsa_cas.push(f.into()),
                None => {
                    eprintln!("--tsa-ca needs a PEM file");
                    return usage();
                }
            },
            "--btc-header" => {
                let Some(h) = args.next().as_deref().and_then(parse_header_arg) else {
                    eprintln!("--btc-header needs HEIGHT:HEX (the 80-byte block header, 160 hex digits)");
                    return usage();
                };
                opts.btc_headers.push(h);
                headers_given = true;
            }
            "--btc-headers" => {
                let Some(f) = args.next() else {
                    eprintln!("--btc-headers needs a file");
                    return usage();
                };
                match load_headers(Path::new(&f)) {
                    Ok(h) => opts.btc_headers.extend(h),
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::from(2);
                    }
                }
                headers_given = true;
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

    let is_bundle = paths.len() == 1 && paths[0].extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
    if (stamp_dir.is_some() || !tsa_cas.is_empty()) && !is_bundle {
        eprintln!("--check-timestamps and --tsa-ca take one export bundle (.json)");
        return ExitCode::from(2);
    }
    if headers_given && !is_bundle {
        eprintln!("--btc-header and --btc-headers take one export bundle (.json)");
        return ExitCode::from(2);
    }
    if !tsa_cas.is_empty() && stamp_dir.is_none() {
        eprintln!("--tsa-ca goes with --check-timestamps DIR");
        return ExitCode::from(2);
    }
    let mut report = if is_bundle {
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
        let mut r = verify::verify_bundle(&bytes, &opts);
        if let Some(dir) = &stamp_dir
            && let Err(e) = check_timestamps(&bytes, dir, &tsa_cas, &mut r, quiet)
        {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
        r
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
    report.findings.sort_by_key(|f| std::cmp::Reverse(f.level));
    if !quiet {
        print_report(&report, list);
    }
    let code = report.exit_code();
    println!("{}", report.verdict());
    println!("{}", report.not_proven());
    ExitCode::from(code as u8)
}

/// `--check-timestamps`: each RFC 3161 token written to `dir` with what was
/// stored beside it, and checked with `openssl ts -verify` at the token's
/// own time against the roots given with `--tsa-ca` (or, saying so, the
/// chain the server stored). Results are added to the report.
fn check_timestamps(json: &[u8], dir: &Path, cas: &[PathBuf], rep: &mut Report, quiet: bool) -> Result<(), String> {
    use evidence::verify::{Finding, Where};
    let Ok(b) = verify::bundle::parse(json) else { return Ok(()) };
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let roots = if cas.is_empty() {
        None
    } else {
        let mut all = Vec::new();
        for c in cas {
            all.extend(std::fs::read(c).map_err(|e| format!("{}: {e}", c.display()))?);
            all.push(b'\n');
        }
        let p = dir.join("trusted-roots.pem");
        std::fs::write(&p, all).map_err(|e| format!("{}: {e}", p.display()))?;
        Some(p)
    };
    let openssl = std::process::Command::new("openssl").arg("version").output().is_ok_and(|o| o.status.success());
    let mut add = |level: Level, at: String, what: String| rep.findings.push(Finding { level, at: Where { source: at, ..Default::default() }, what });
    if !quiet {
        println!("time-stamp tokens written to {}", dir.display());
    }
    for (i, t) in b.timestamps.iter().enumerate() {
        let at = format!("time-stamp {i} ({})", t.authority);
        if t.kind == verify::bundle::KIND_OTS {
            // For other OpenTimestamps tools (e.g. `ots info`); the digest it
            // is over is the imprint stored beside it.
            if let Some(token) = evidence::b64::decode_vec(&t.token) {
                let p = dir.join(format!("stamp-{i}.ots"));
                std::fs::write(&p, &token).map_err(|e| format!("{}: {e}", p.display()))?;
                if !quiet {
                    println!("  {at}: OpenTimestamps proof over digest {}: {}", t.imprint, p.display());
                }
            }
            continue;
        }
        if t.kind != verify::bundle::KIND_RFC3161 {
            continue;
        }
        let Some(token) = evidence::b64::decode_vec(&t.token) else { continue };
        let Ok(info) = evidence::tsp::tst_info(&token) else { continue };
        let w = |name: String, bytes: &[u8]| -> Result<PathBuf, String> {
            let p = dir.join(name);
            std::fs::write(&p, bytes).map_err(|e| format!("{}: {e}", p.display()))?;
            Ok(p)
        };
        let tsr = w(format!("stamp-{i}.tsr"), &token)?;
        let chain = match t.certs.as_deref().and_then(evidence::b64::decode_vec) {
            Some(c) => Some(w(format!("stamp-{i}-chain.pem"), &c)?),
            None => None,
        };
        if let Some(c) = t.revocation.as_deref().and_then(evidence::b64::decode_vec) {
            let p = w(format!("stamp-{i}.crl"), &c)?;
            if !quiet {
                println!("  {at}: revocation list as fetched when stamped ({}): {} (openssl crl -in FILE -noout -text; add -inform DER if it is binary)", t.revocation_from.as_deref().unwrap_or("?"), p.display());
            }
        }
        let (ca, outside) = match (&roots, &chain) {
            (Some(r), _) => (r.clone(), true),
            (None, Some(c)) => (c.clone(), false),
            (None, None) => {
                add(Level::Note, at, "signature not checked: no certificate to check it against (give the authority's root with --tsa-ca)".into());
                continue;
            }
        };
        let mut args: Vec<String> = vec!["ts".into(), "-verify".into(), "-in".into(), tsr.display().to_string(), "-digest".into(), t.imprint.clone(), "-CAfile".into(), ca.display().to_string()];
        if outside && let Some(c) = &chain {
            args.extend(["-untrusted".into(), c.display().to_string()]);
        }
        args.extend(["-attime".into(), (info.gen_time_ms / 1000).to_string()]);
        if !quiet {
            println!("  {at}: openssl {}", args.join(" "));
        }
        if !openssl {
            add(Level::Note, at, "signature not checked here: openssl not found; run the command shown".into());
            continue;
        }
        let out = std::process::Command::new("openssl").args(&args).output().map_err(|e| format!("openssl: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        if out.status.success() && text.contains("Verification: OK") {
            let whose = if outside {
                "the roots given with --tsa-ca".to_string()
            } else {
                "the certificate the server stored with it (it came from the server: give the authority's own root with --tsa-ca for an outside check)".to_string()
            };
            add(Level::Note, at, format!("openssl: the token's signature checks out against {whose}, at the token's time {}", verify::fmt_utc(info.gen_time_ms)));
        } else {
            // openssl's reason ("Verify error:...", or its error line), else its last line.
            let why = text
                .lines()
                .find_map(|l| l.split_once("Verify error:").map(|(_, r)| r))
                .or_else(|| text.lines().find(|l| l.contains("error:")).map(|l| l.rsplit(':').next().unwrap_or(l)))
                .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
                .unwrap_or("no output")
                .trim()
                .to_string();
            add(Level::Fail, at, format!("openssl ts -verify failed: {why}"));
        }
    }
    Ok(())
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
