//! Bakes the kit's ESP-NOW link key into the build, or records that there is
//! none. The key never lives in the repository (it is public): it is a local,
//! git-ignored file of 64 hex digits, made with `cargo run -p link-key -- new`.
//!
//! Which file:
//! - `ROTORSCOPE_LINK_KEY_FILE=<path>`: that file (it must exist);
//! - `ROTORSCOPE_LINK_KEY_FILE=none`: no key, even if `tools/link.key` exists;
//! - unset: `tools/link.key` at the repository root if it exists, else no key.
//!
//! Output (in `OUT_DIR`, inside the git-ignored target directory):
//! `KIT_KEY: Option<[u8; 32]>` and `KEY_ID` (8 hex digits of
//! HMAC(key, "rotorscope link key id"), safe to print; it identifies the key
//! without revealing it). The key itself is never printed.

#[path = "src/hmac.rs"]
mod hmac;

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=ROTORSCOPE_LINK_KEY_FILE");
    println!("cargo:rerun-if-changed=src/hmac.rs");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let tools = manifest.join("../../tools");
    let default = tools.join("link.key");
    let path = match std::env::var("ROTORSCOPE_LINK_KEY_FILE") {
        Ok(v) if v == "none" => None,
        Ok(v) if !v.is_empty() => {
            let p = PathBuf::from(v);
            if !p.is_file() {
                panic!("ROTORSCOPE_LINK_KEY_FILE={}: no such file", p.display());
            }
            Some(p)
        }
        _ => {
            // Notice a key file appearing or disappearing (the directory's
            // own timestamp changes then).
            if tools.is_dir() {
                println!("cargo:rerun-if-changed={}", tools.display());
            }
            default.is_file().then_some(default)
        }
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("link_key.rs");
    let code = match &path {
        None => {
            println!(
                "cargo:warning=airlink: no link key (tools/link.key): this build's ESP-NOW link is NOT authenticated. See docs/ESPNOW.md \"Securing the link\"."
            );
            "/// No key: unauthenticated ESP-NOW (docs/ESPNOW.md).\npub const KIT_KEY: Option<[u8; 32]> = None;\n/// See [`KIT_KEY`].\npub const KEY_ID: &str = \"none\";\n".to_string()
        }
        Some(p) => {
            println!("cargo:rerun-if-changed={}", p.display());
            let text = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            let Some(key) = parse_hex_key(&text) else {
                // Never echo the contents: it may be a real key with a typo.
                panic!(
                    "{}: not a link key (expected exactly 64 hex digits). Make one with: cargo run -p link-key -- new",
                    p.display()
                );
            };
            let id = hmac::Hmac::new(&key).mac(&[b"rotorscope link key id"]);
            let id = format!("{:02x}{:02x}{:02x}{:02x}", id[0], id[1], id[2], id[3]);
            println!("cargo:warning=airlink: ESP-NOW link key {id} from {}", p.display());
            let bytes: Vec<String> = key.iter().map(|b| format!("0x{b:02x}")).collect();
            format!(
                "/// The kit's link key (from a local file, never committed).\npub const KIT_KEY: Option<[u8; 32]> = Some([{}]);\n/// See [`KIT_KEY`].\npub const KEY_ID: &str = \"{id}\";\n",
                bytes.join(", ")
            )
        }
    };
    std::fs::write(&out, code).unwrap();
}

/// 64 hex digits, whitespace allowed anywhere (a trailing newline, or the
/// digits split into groups).
fn parse_hex_key(text: &str) -> Option<[u8; 32]> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() != 64 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, pair) in digits.chunks(2).enumerate() {
        let s = std::str::from_utf8(pair).ok()?;
        key[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some(key)
}
