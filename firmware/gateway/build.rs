//! Bench fence (crates/link/src/benchfence.rs): a build with a bench-only
//! feature stops unless `ROTORSCOPE_BENCH=1`, and the list goes into the
//! hello line.

#[path = "../../crates/link/src/benchfence.rs"]
mod benchfence;

fn main() {
    println!("cargo:rerun-if-changed=../../crates/link/src/benchfence.rs");
    benchfence::run("gateway", &["faulttest", "gwforge"], &[]);
}
