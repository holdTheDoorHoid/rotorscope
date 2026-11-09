//! Compiles the vendored opendroneid-core-c and its GB 46750 variant (both
//! Apache-2.0) plus tiny shims that report struct sizes, so the Rust mirrors
//! can be checked against the C ABI.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib = root.join("../../third_party/opendroneid-core-c/libopendroneid");
    let shim = root.join("csrc/shim.c");
    let cn = root.join("../../third_party/opendroneid-core-c/libopendroneidcn");
    let shim_cn = root.join("csrc/shim_cn.c");

    for f in ["opendroneid.c", "wifi.c", "opendroneid.h", "odid_wifi.h"] {
        println!("cargo:rerun-if-changed={}", lib.join(f).display());
    }
    for f in [
        cn.join("opendroneidcn.c"),
        cn.join("opendroneidcn.h"),
        shim.clone(),
        shim_cn.clone(),
    ] {
        println!("cargo:rerun-if-changed={}", f.display());
    }

    cc::Build::new()
        .file(lib.join("opendroneid.c"))
        .file(lib.join("wifi.c"))
        .file(&shim)
        .include(&lib)
        .define("ODID_DISABLE_PRINTF", None)
        .warnings(false)
        .compile("opendroneid");

    // GB 46750-2025 variant: separate build so its header never meets opendroneid.h.
    cc::Build::new()
        .file(cn.join("opendroneidcn.c"))
        .file(&shim_cn)
        .include(&cn)
        .warnings(false)
        .compile("opendroneidcn");
}
