//! Link script plus a guard that the chip feature matches the target, so a
//! wrong combination fails here with a clear message instead of deep inside esp-hal;
//! and the bench fence (crates/link/src/benchfence.rs): a build with a
//! bench-only feature or build switch stops unless `ROTORSCOPE_BENCH=1`, and
//! the list goes into the hello line.

#[path = "../../crates/link/src/benchfence.rs"]
mod benchfence;

fn main() {
    println!("cargo:rerun-if-changed=../../crates/link/src/benchfence.rs");
    benchfence::run("node-wifi", &["faulttest"], &["ROTORSCOPE_BENCH_CAN", "ROTORSCOPE_BENCH_FLIP"]);
    let target = std::env::var("TARGET").unwrap_or_default();
    let s3 = std::env::var_os("CARGO_FEATURE_ESP32S3").is_some();
    let c5 = std::env::var_os("CARGO_FEATURE_ESP32C5").is_some();

    match (s3, c5) {
        (true, false) if target == "xtensa-esp32s3-none-elf" => {}
        (false, true) if target == "riscv32imac-unknown-none-elf" => {}
        (false, false) => panic!(
            "no chip selected: build from firmware/node-wifi/s3 or .../c5 with `cargo fw` (see README.md)"
        ),
        _ => panic!(
            "chip feature does not match target `{target}`: build from firmware/node-wifi/s3 or .../c5 with `cargo fw`"
        ),
    }

    if target.starts_with("xtensa-") {
        check_xtensa_linker(&target);
    }

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
