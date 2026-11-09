//! Compiles the vendored opendroneid-core-c (Apache-2.0) plus a tiny shim that
//! reports struct sizes, so the Rust mirrors can be checked against the C ABI.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib = root.join("../../third_party/opendroneid-core-c/libopendroneid");
    let shim = root.join("csrc/shim.c");

    for f in ["opendroneid.c", "wifi.c", "opendroneid.h", "odid_wifi.h"] {
        println!("cargo:rerun-if-changed={}", lib.join(f).display());
    }
    println!("cargo:rerun-if-changed={}", shim.display());

    cc::Build::new()
        .file(lib.join("opendroneid.c"))
        .file(lib.join("wifi.c"))
        .file(&shim)
        .include(&lib)
        .define("ODID_DISABLE_PRINTF", None)
        .warnings(false)
        .compile("opendroneid");
}
