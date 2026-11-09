//! Link script, a chip/target guard (as the nodes have), and station mode.
//!
//! Station mode (PLAN.md O5) is compiled in only when the owner has created
//! `wifi.local.toml` next to this file. That file is gitignored and holds the
//! network name and password; this script turns it into the `hub_station` cfg
//! plus two compile-time environment variables. Without the file, no station
//! code is built at all, so a kit built from a clean checkout never tries to
//! join anything.

use std::path::Path;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    let s3 = std::env::var_os("CARGO_FEATURE_ESP32S3").is_some();
    let c5 = std::env::var_os("CARGO_FEATURE_ESP32C5").is_some();

    match (s3, c5) {
        (true, false) if target == "xtensa-esp32s3-none-elf" => {}
        (false, true) if target == "riscv32imac-unknown-none-elf" => {}
        (false, false) => panic!(
            "no chip selected: build from firmware/hub/s3 or .../c5 with `cargo fw` (see README.md)"
        ),
        _ => panic!(
            "chip feature does not match target `{target}`: build from firmware/hub/s3 or .../c5 with `cargo fw`"
        ),
    }

    if target.starts_with("xtensa-") {
        check_xtensa_linker(&target);
    }

    station_mode();
    build_time();

    // linkall.x must be the last linker script.
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

/// The Xtensa GCC linker only exists after `. ~/export-esp.sh`; say so up front.
fn check_xtensa_linker(target: &str) {
    println!("cargo:rerun-if-env-changed=PATH");
    let chip = target.trim_start_matches("xtensa-").trim_end_matches("-none-elf");
    let linker = format!("xtensa-{chip}-elf-gcc");
    if std::process::Command::new(&linker).arg("--version").output().is_err() {
        panic!("`{linker}` not on PATH: run `. ~/export-esp.sh` first (installed by `espup install`)");
    }
}

#[derive(serde::Deserialize)]
struct WifiLocal {
    ssid: String,
    psk: String,
}

fn station_mode() {
    println!("cargo::rustc-check-cfg=cfg(hub_station)");
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let path = Path::new(&dir).join("wifi.local.toml");
    // Cargo reruns this script when the file appears, changes or disappears.
    // (Watching a missing file makes cargo rerun the script on every build;
    // that only costs this script's run, the output does not change.)
    println!("cargo:rerun-if-changed={}", path.display());

    let Ok(text) = std::fs::read_to_string(&path) else {
        return; // No file: station mode is compiled out.
    };
    let cfg: WifiLocal = match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => panic!("{}: expected `ssid = \"...\"` and `psk = \"...\"`: {e}", path.display()),
    };
    if cfg.ssid.is_empty() || cfg.ssid.len() > 32 {
        panic!("{}: ssid must be 1-32 bytes", path.display());
    }
    // WPA2-PSK passphrases are 8-63 characters; empty means an open network.
    if !cfg.psk.is_empty() && !(8..=63).contains(&cfg.psk.len()) {
        panic!("{}: psk must be empty (open network) or 8-63 characters", path.display());
    }
    println!("cargo:rustc-cfg=hub_station");
    println!("cargo:rustc-env=HUB_STA_SSID={}", cfg.ssid);
    println!("cargo:rustc-env=HUB_STA_PSK={}", cfg.psk);
    // Deliberately not printed: build logs get pasted into issues.
    println!("cargo:warning=station mode ON (wifi.local.toml found; ssid and psk compiled in)");
}

/// Build time as Unix milliseconds. Used only by the `demo` feature as a
/// stand-in clock so CoT events carry a plausible time when there is no GPS.
fn build_time() {
    if std::env::var_os("CARGO_FEATURE_DEMO").is_none() {
        return;
    }
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    println!("cargo:rustc-env=HUB_BUILD_UNIX_MS={ms}");
}
