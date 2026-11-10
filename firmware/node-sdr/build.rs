//! Link scripts, a target check, and the PHY ABI guard.
//!
//! `src/phy.rs` calls private entry points of Espressif's closed `libphy.a`
//! with signatures recovered from one specific build of it (see the evidence
//! there). A different blob could take different arguments, write different
//! registers, or not exist, and nothing would fail until the radio misbehaved.
//! So the build refuses any `libphy.a` but the one the ABI was checked
//! against, in the spirit of CapnBry/widscan58's `check_phy_abi.sh`. This is
//! the same file C5VRX pins (its CMakeLists.txt, `C5VRX_PHY_PINNED`): ESP-IDF
//! 6.0.2's C5 libphy, version string `edb400d6` (built Mar 10 2026), shipped
//! in `esp-wifi-sys-esp32c5` 0.3.0 (pinned with `=` in Cargo.toml).
//!
//! The firmware repeats the check at run time against the version string the
//! linked blob reports (`phy::abi_ok`).

use std::path::{Path, PathBuf};

/// SHA-256 of `esp-wifi-sys-esp32c5-0.3.0/libs/libphy.a`.
const PHY_SHA256: &str = "dbf33c418c8d408d4005c849d12a1432deea82e2e5e57de3c8ddf914d104fffb";

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target != "riscv32imac-unknown-none-elf" {
        panic!("node-sdr is ESP32-C5 only: build from firmware/node-sdr/c5 with `cargo fw` (target `{target}`)");
    }

    check_phy_blob();

    let ld = Path::new(env!("CARGO_MANIFEST_DIR")).join("ld");
    println!("cargo:rustc-link-search={}", ld.display());
    println!("cargo:rerun-if-changed=ld/sdr_bank.x");
    // The bank script INSERTs itself into esp-hal's layout; linkall.x must be
    // the last linker script.
    println!("cargo:rustc-link-arg=-Tsdr_bank.x");
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    // A map file next to the ELF, for the memory figures in README.md.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    if let Some(profile_dir) = out.ancestors().nth(3) {
        println!("cargo:rustc-link-arg=-Map={}", profile_dir.join("node-sdr.map").display());
    }
}

fn check_phy_blob() {
    println!("cargo:rerun-if-env-changed=NODE_SDR_ALLOW_UNVERIFIED_PHY");
    let allow = std::env::var_os("NODE_SDR_ALLOW_UNVERIFIED_PHY").is_some();
    let Some(path) = find_libphy() else {
        println!(
            "cargo:warning=node-sdr: could not find esp-wifi-sys-esp32c5-0.3.0/libs/libphy.a under CARGO_HOME to check \
             its SHA-256; the run-time version check still applies"
        );
        return;
    };
    println!("cargo:rerun-if-changed={}", path.display());
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let got = hex(&<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    if got != PHY_SHA256 {
        let msg = format!(
            "node-sdr: {} has SHA-256 {got}, not the {PHY_SHA256} whose private ABI src/phy.rs was checked against. \
             Re-verify every extern in src/phy.rs against the new blob (disassemble it) before updating PHY_SHA256 \
             in build.rs and PHY_VERSION in src/phy.rs. NODE_SDR_ALLOW_UNVERIFIED_PHY=1 builds anyway.",
            path.display()
        );
        if allow {
            println!("cargo:warning={msg}");
        } else {
            panic!("{msg}");
        }
    }
}

fn find_libphy() -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))?;
    let src = home.join("registry").join("src");
    for index in std::fs::read_dir(src).ok()?.flatten() {
        let p = index.path().join("esp-wifi-sys-esp32c5-0.3.0").join("libs").join("libphy.a");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
