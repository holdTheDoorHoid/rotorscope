//! Build-time checks:
//!
//! - **Bench fence** (crates/link/src/benchfence.rs): a build with a
//!   bench-only feature (fake drones and nodes, loop-back tests, the test
//!   position, `faulttest`, the fake rotor sound and the audio dump, ...) stops unless `ROTORSCOPE_BENCH=1`, and the
//!   list goes into the hello line, the evidence boot event and the screen.
//! - **Evidence dev key** (crates/evidence/src/devkey.rs): a set but
//!   malformed `ROTORSCOPE_DEV_KEY` stops the build (the firmware used to
//!   fall back to the published test key without a word). Never printed.

#[path = "../../crates/link/src/benchfence.rs"]
mod benchfence;
#[path = "../../crates/evidence/src/devkey.rs"]
mod devkey;

fn main() {
    println!("cargo:rerun-if-changed=../../crates/link/src/benchfence.rs");
    println!("cargo:rerun-if-changed=../../crates/evidence/src/devkey.rs");
    benchfence::run(
        "hub-p4",
        &[
            "selftest",
            "fakenode",
            "fakedrone",
            "benchlog",
            "gpsloop",
            "gpsloop-m10",
            "webheadloop",
            "embedpack",
            "modesloop",
            "faulttest",
            "testpoint",
            // The microphone (src/ear.rs): a made-up rotor sound mixed in on
            // the chip, and raw room audio dumped over the USB console.
            "earfake",
            "eardump",
        ],
        &[],
    );
    println!("cargo:rerun-if-env-changed=ROTORSCOPE_DEV_KEY");
    // Set but not UTF-8 counts as malformed (empty), not as unset.
    let key = std::env::var_os("ROTORSCOPE_DEV_KEY").map(|v| v.into_string().unwrap_or_default());
    if let Err(msg) = devkey::check(key.as_deref()) {
        panic!("hub-p4: {msg}");
    }
}
