//! Which link key a build gets (used by `build.rs`; compiled into the
//! library only for its unit tests).
//!
//! A firmware image without a key accepts every ESP-NOW frame from anyone
//! nearby, so a firmware build (target OS `none`) must either find a key or
//! be told on purpose that it is an open kit (`ROTORSCOPE_LINK_KEY_FILE=none`).
//! Host builds (unit tests, host tools, a fresh clone's `cargo test`) never
//! talk to a radio and build without a key when there is none.

/// What `build.rs` compiles in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Read the key from this file (it must exist and hold 64 hex digits).
    File(std::path::PathBuf),
    /// No key, on purpose (`ROTORSCOPE_LINK_KEY_FILE=none`).
    OpenOnPurpose,
    /// No key, and none is needed: a host build.
    OpenHost,
}

/// The message a firmware build without a key stops with.
pub const NO_KEY: &str = "airlink: no ESP-NOW link key, and this is a firmware build.\n\
     Without a key the kit accepts every ESP-NOW frame from anyone nearby (docs/ESPNOW.md \"Securing the link\").\n\
     Make the kit key once:  cargo run -p link-key -- new   (writes tools/link.key; then flash the gateway and every node)\n\
     Or build an open kit on purpose:  ROTORSCOPE_LINK_KEY_FILE=none cargo ...";

/// `env`: `ROTORSCOPE_LINK_KEY_FILE`. `default`: `tools/link.key`, if it is
/// a file. `firmware`: the target OS is `none`.
pub fn choose(env: Option<&str>, default: Option<std::path::PathBuf>, firmware: bool) -> Result<Choice, std::string::String> {
    match env {
        Some("none") => Ok(Choice::OpenOnPurpose),
        Some(v) if !v.is_empty() => {
            let p = std::path::PathBuf::from(v);
            if p.is_file() {
                Ok(Choice::File(p))
            } else {
                Err(std::format!("ROTORSCOPE_LINK_KEY_FILE={}: no such file", p.display()))
            }
        }
        _ => match default {
            Some(p) => Ok(Choice::File(p)),
            None if firmware => Err(NO_KEY.into()),
            None => Ok(Choice::OpenHost),
        },
    }
}

/// The compile target is bare metal (every Rotorscope firmware: Xtensa
/// `xtensa-esp32s3-none-elf`, RISC-V `riscv32*-unknown-none-elf`).
pub fn is_firmware(target_os: Option<&str>) -> bool {
    target_os == Some("none")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn firmware_without_a_key_is_an_error() {
        let e = choose(None, None, true).unwrap_err();
        assert!(e.contains("cargo run -p link-key -- new") && e.contains("ROTORSCOPE_LINK_KEY_FILE=none"), "{e}");
        // An empty variable is the same as unset.
        assert!(choose(Some(""), None, true).is_err());
    }

    #[test]
    fn host_without_a_key_builds_open() {
        assert_eq!(choose(None, None, false), Ok(Choice::OpenHost));
        assert_eq!(choose(Some(""), None, false), Ok(Choice::OpenHost));
    }

    #[test]
    fn explicit_open_kit() {
        assert_eq!(choose(Some("none"), None, true), Ok(Choice::OpenOnPurpose));
        // `none` wins over a key file that exists.
        assert_eq!(choose(Some("none"), Some(PathBuf::from("/k")), true), Ok(Choice::OpenOnPurpose));
    }

    #[test]
    fn default_key_file_is_used() {
        let k = PathBuf::from("/repo/tools/link.key");
        assert_eq!(choose(None, Some(k.clone()), true), Ok(Choice::File(k.clone())));
        assert_eq!(choose(None, Some(k.clone()), false), Ok(Choice::File(k)));
    }

    #[test]
    fn named_key_file_must_exist() {
        let e = choose(Some("/nonexistent/rotorscope/link.key"), None, true).unwrap_err();
        assert!(e.contains("no such file"), "{e}");
        // Also on the host: a named file that is missing is a mistake.
        assert!(choose(Some("/nonexistent/rotorscope/link.key"), None, false).is_err());
        let here = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert_eq!(choose(here.to_str(), None, true), Ok(Choice::File(here)));
    }

    #[test]
    fn firmware_targets() {
        assert!(is_firmware(Some("none")));
        assert!(!is_firmware(Some("linux")));
        assert!(!is_firmware(Some("macos")));
        assert!(!is_firmware(None));
    }
}
