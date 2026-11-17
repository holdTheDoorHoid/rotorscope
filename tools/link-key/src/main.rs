//! The kit's ESP-NOW link key: one random 32-byte key shared by the gateway
//! and every ESP-NOW node of one kit (docs/ESPNOW.md, "Securing the link").
//!
//! ```text
//! cargo run -p link-key -- new          # write tools/link.key (refuses to overwrite)
//! cargo run -p link-key -- new --force  # replace it (then reflash EVERY board)
//! cargo run -p link-key -- id [FILE]    # print the key's ID (default tools/link.key)
//! ```
//!
//! The file is 64 hex digits, readable only by you, and git-ignored: the
//! repository is public, so the key must never be committed. The key is never
//! printed; the ID (8 hex digits) names it without revealing it, and boards
//! print the same ID at boot so you can check they match.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn default_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../link.key")
}

fn id_of(key: &[u8; 32]) -> String {
    airlink::auth::Key::new(key).id().iter().map(|b| format!("{b:02x}")).collect()
}

fn read_key(path: &PathBuf) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() != 64 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return Err(format!("{}: not a link key (expected 64 hex digits)", path.display()));
    }
    let mut key = [0u8; 32];
    for (i, pair) in digits.chunks(2).enumerate() {
        key[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    Ok(key)
}

fn new_key(path: &PathBuf, force: bool) -> Result<String, String> {
    if path.exists() && !force {
        return Err(format!(
            "{} already exists (ID {}). Keep it: every board of the kit uses it. \
             To replace it on purpose: new --force, then reflash the gateway and every ESP-NOW node.",
            path.display(),
            read_key(path).map(|k| id_of(&k)).unwrap_or_else(|_| "unreadable".into())
        ));
    }
    let mut key = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut key))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(hex.as_bytes()).and_then(|()| f.write_all(b"\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(id_of(&key))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("new") => {
            let force = args.iter().any(|a| a == "--force");
            let path = args.iter().skip(1).find(|a| !a.starts_with("--")).map(PathBuf::from).unwrap_or_else(default_path);
            new_key(&path, force).map(|id| {
                format!(
                    "wrote {} (key ID {id}). Never commit it (tools/link.key is git-ignored); back it up somewhere private.\n\
                     Now rebuild and flash the gateway and every ESP-NOW node; each prints \"key {id}\" at boot.",
                    path.display()
                )
            })
        }
        Some("id") => {
            let path = args.get(1).map(PathBuf::from).unwrap_or_else(default_path);
            read_key(&path).map(|k| id_of(&k))
        }
        _ => Err("usage: link-key new [--force] [FILE] | link-key id [FILE]".into()),
    };
    match r {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("link-key: {e}");
            ExitCode::FAILURE
        }
    }
}
