//! Make the FLARM legacy key file from a SoftRF checkout.
//!
//! ```text
//! cargo run -p flarm-keys -- SOFTRF_DIR_OR_LEGACY_H [--out FILE] [--force]
//! ```
//!
//! Reads the `LEGACY_KEY1` .. `LEGACY_KEY5` constants from SoftRF's
//! `Legacy.h` (and `Legacy.cpp` beside it, in case a version moves them)
//! and writes them in the format `flarm::Keys::parse` reads (see
//! `crates/flarm/src/keys.rs`), by default to `tools/flarm.keys`. For the
//! hub, copy that file to the SD card as `ROTORSCP/FLARMKEY.TXT`.
//!
//! The owner may use these keys (2026-11-17); the repository is public and
//! they must never be published. So this tool never prints a key value,
//! writes the file readable only by you, refuses to write anywhere git
//! would not ignore, and refuses to overwrite an existing file without
//! `--force`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Names and word counts, in file order.
const KEYS: [(&str, usize); 5] = [
    ("key1", 8),
    ("key2", 1),
    ("key3", 1),
    ("key4", 1),
    ("key5", 4),
];

fn default_out() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .join("flarm.keys")
}

/// `Legacy.h` from a file path or a SoftRF checkout (or a subdirectory).
fn find_header(p: &Path) -> Result<PathBuf, String> {
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    let tails = [
        "software/firmware/source/SoftRF/src/protocol/radio/Legacy.h",
        "firmware/source/SoftRF/src/protocol/radio/Legacy.h",
        "source/SoftRF/src/protocol/radio/Legacy.h",
        "SoftRF/src/protocol/radio/Legacy.h",
        "src/protocol/radio/Legacy.h",
        "protocol/radio/Legacy.h",
        "Legacy.h",
    ];
    tails
        .iter()
        .map(|t| p.join(t))
        .find(|c| c.is_file())
        .ok_or_else(|| {
            format!(
                "{}: no Legacy.h found (give the SoftRF checkout or the file)",
                p.display()
            )
        })
}

/// The value text of `#define NAME ...`, with `\` line continuations joined.
fn define(lines: &[String], name: &str) -> Option<String> {
    let mut it = lines.iter();
    while let Some(l) = it.next() {
        let t = l.trim_start();
        let Some(rest) = t.strip_prefix("#define") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(after) = rest.strip_prefix(name) else {
            continue;
        };
        if !after.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let mut value = String::from(after);
        while value.trim_end().ends_with('\\') {
            let v = value.trim_end();
            value = v[..v.len() - 1].to_string();
            match it.next() {
                Some(next) => value.push_str(next),
                None => break,
            }
        }
        // drop comments
        if let Some(i) = value.find("/*") {
            value.truncate(i);
        }
        if let Some(i) = value.find("//") {
            value.truncate(i);
        }
        return Some(value);
    }
    None
}

/// Every `0x...` word in `text`.
fn hex_words(text: &str) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'0' && (b[i + 1] == b'x' || b[i + 1] == b'X') {
            let start = i + 2;
            let mut end = start;
            while end < b.len() && b[end].is_ascii_hexdigit() {
                end += 1;
            }
            let digits = &text[start..end];
            if digits.is_empty() || digits.len() > 8 {
                return Err("a constant is not a 32-bit hex word".into());
            }
            out.push(u32::from_str_radix(digits, 16).map_err(|_| "bad hex constant".to_string())?);
            i = end;
        } else {
            i += 1;
        }
    }
    Ok(out)
}

fn extract(header: &Path) -> Result<String, String> {
    let mut text =
        std::fs::read_to_string(header).map_err(|e| format!("{}: {e}", header.display()))?;
    if let Some(cpp) = header.parent().map(|d| d.join("Legacy.cpp"))
        && let Ok(more) = std::fs::read_to_string(&cpp)
    {
        text.push('\n');
        text.push_str(&more);
    }
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut out = String::from(
        "# FLARM legacy keys for Rotorscope's flarm crate, extracted from SoftRF's Legacy.h.\n\
         # The owner may use them (2026-11-17); they must NEVER be committed, printed,\n\
         # logged or sent between boards. On the hub: SD card, ROTORSCP/FLARMKEY.TXT.\n",
    );
    for (i, (name, count)) in KEYS.iter().enumerate() {
        let macro_name = format!("LEGACY_KEY{}", i + 1);
        let value = define(&lines, &macro_name)
            .ok_or_else(|| format!("{macro_name} not found in {}", header.display()))?;
        let words = hex_words(&value).map_err(|e| format!("{macro_name}: {e}"))?;
        if words.len() != *count {
            return Err(format!(
                "{macro_name}: expected {count} words, found {}",
                words.len()
            ));
        }
        let joined: Vec<String> = words.iter().map(|w| format!("{w:08x}")).collect();
        out.push_str(&format!("{name} = {}\n", joined.join(" ")));
    }
    Ok(out)
}

/// Refuse a destination git would track.
fn check_ignored(out: &Path) -> Result<(), String> {
    let dir = out
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let inside = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !inside {
        return Ok(());
    }
    let name = out.file_name().ok_or("output has no file name")?;
    let ignored = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["check-ignore", "-q", "--no-index"])
        .arg(name)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ignored {
        Ok(())
    } else {
        Err(format!(
            "{} is inside a git repository and NOT ignored: refusing to write keys there. \
             Use the default (tools/flarm.keys) or a path outside the repository.",
            out.display()
        ))
    }
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(text.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn run(args: &[String]) -> Result<String, String> {
    let force = args.iter().any(|a| a == "--force");
    let mut out = default_out();
    let mut src = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--force" => {}
            "--out" => out = PathBuf::from(it.next().ok_or("--out needs a file")?),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => src = Some(PathBuf::from(s)),
        }
    }
    let src = src.ok_or("usage: flarm-keys SOFTRF_DIR_OR_LEGACY_H [--out FILE] [--force]")?;
    let header = find_header(&src)?;
    let text = extract(&header)?;
    // What we write must be what the decoder reads.
    flarm::Keys::parse(&text)
        .map_err(|e| format!("internal: extracted keys do not parse ({e:?})"))?;
    if out.exists() && !force {
        return Err(format!(
            "{} already exists; --force to replace it",
            out.display()
        ));
    }
    check_ignored(&out)?;
    write_private(&out, &text)?;
    Ok(format!(
        "wrote {} ({} keys from {}; values not shown).\n\
         Never commit it (git-ignored). For the hub, copy it to the SD card as ROTORSCP/FLARMKEY.TXT.\n\
         Real-key tests: FLARM_KEYS_FILE={} cargo test -p flarm",
        out.display(),
        KEYS.len(),
        header.display(),
        out.display()
    ))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("flarm-keys: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A made-up header in SoftRF's layout (not FLARM's keys).
    const HEADER: &str = "\
#define LEGACY_KEY1 { 0x00000001, 0x00000002, 0x00000003, 0x00000004, \\
                      0x00000005, 0x00000006, 0x00000007, 0x00000008 }
#define LEGACY_KEY2 0x0000000a
#define LEGACY_KEY3 0x0000000b /* comment 0x99 */
#define LEGACY_KEY4 0x0000000C
#define LEGACY_KEY5 { 0x10, 0x20, 0x30, 0x40 }
#define LEGACY_KEY10 0xffffffff
";

    #[test]
    fn extracts_continued_defines_and_skips_comments() {
        let dir = std::env::temp_dir().join(format!("flarm-keys-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let h = dir.join("Legacy.h");
        std::fs::write(&h, HEADER).unwrap();
        let text = extract(&h).unwrap();
        assert!(text.contains(
            "key1 = 00000001 00000002 00000003 00000004 00000005 00000006 00000007 00000008\n"
        ));
        assert!(text.contains("key3 = 0000000b\n"));
        assert!(text.contains("key4 = 0000000c\n"));
        assert!(text.contains("key5 = 00000010 00000020 00000030 00000040\n"));
        assert!(flarm::Keys::parse(&text).is_ok());
        assert_eq!(find_header(&dir).unwrap(), h);
        // A missing constant is an error naming the macro only.
        std::fs::write(&h, HEADER.replace("#define LEGACY_KEY4 0x0000000C\n", "")).unwrap();
        let e = extract(&h).unwrap_err();
        assert!(e.starts_with("LEGACY_KEY4 not found"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_a_tracked_path_inside_the_repository() {
        let tracked = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml.keys");
        assert!(check_ignored(&tracked).is_err());
        assert!(check_ignored(&default_out()).is_ok());
    }
}
