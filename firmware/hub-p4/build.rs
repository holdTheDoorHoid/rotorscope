//! Build-time checks:
//!
//! - **Bench fence** (crates/link/src/benchfence.rs): a build with a
//!   bench-only feature (fake drones and nodes, loop-back tests, the test
//!   position, `faulttest`, ...) stops unless `ROTORSCOPE_BENCH=1`, and the
//!   list goes into the hello line, the evidence boot event and the screen.
//! - **Evidence dev key** (crates/evidence/src/devkey.rs): a set but
//!   malformed `ROTORSCOPE_DEV_KEY` stops the build (the firmware used to
//!   fall back to the published test key without a word). Never printed.
//! - **Source commit** (crates/evidence/src/buildid.rs): `ROTORSCOPE_GIT`,
//!   the commit and whether tracked files were changed, for the hello line,
//!   the evidence boot event and a string in the ELF. Outside a git
//!   checkout it is `unknown`; that never fails the build.

#[path = "../../crates/link/src/benchfence.rs"]
mod benchfence;
#[path = "../../crates/evidence/src/buildid.rs"]
mod buildid;
#[path = "../../crates/evidence/src/devkey.rs"]
mod devkey;

/// Run git in this crate's folder; its trimmed output on success.
fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `ROTORSCOPE_GIT`, and the files whose change should run this again.
fn source_commit() {
    let commit = git(&["rev-parse", "HEAD"]);
    let status = commit.as_ref().and_then(|_| git(&["status", "--porcelain", "--untracked-files=no"]));
    println!("cargo:rustc-env=ROTORSCOPE_GIT={}", buildid::git_field(commit.as_deref(), status.as_deref()));
    // A commit, a checkout or a staged change moves these; an edit to a
    // source file is caught by the folders below (any change in them runs
    // this again, which re-reads the tree state).
    for (arg, files) in [("--git-dir", &["HEAD", "index"][..]), ("--git-common-dir", &["packed-refs", "refs/heads"][..])] {
        if let Some(dir) = git(&["rev-parse", arg]) {
            let dir = std::path::Path::new(dir.trim());
            for f in files {
                let p = dir.join(f);
                if p.exists() {
                    println!("cargo:rerun-if-changed={}", p.display());
                }
            }
        }
    }
    for d in ["src", "Cargo.toml", "Cargo.lock", "../../crates"] {
        println!("cargo:rerun-if-changed={d}");
    }
}

fn main() {
    println!("cargo:rerun-if-changed=../../crates/link/src/benchfence.rs");
    println!("cargo:rerun-if-changed=../../crates/evidence/src/devkey.rs");
    println!("cargo:rerun-if-changed=../../crates/evidence/src/buildid.rs");
    source_commit();
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
        ],
        &[],
    );
    // WP-12: GPS time before this firmware's build date is impossible. The date
    // build.rs last ran (or SOURCE_DATE_EPOCH, for a reproducible build): never
    // later than the real build, so a valid floor.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let built = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()));
    println!("cargo:rustc-env=ROTORSCOPE_BUILD_UNIX_S={built}");
    // Bench (`gpsloop`): which scripted GPS scenario the emulated receiver plays.
    println!("cargo:rerun-if-env-changed=GPSLOOP_SCENARIO");
    println!("cargo:rerun-if-env-changed=ROTORSCOPE_DEV_KEY");
    // Set but not UTF-8 counts as malformed (empty), not as unset.
    let key = std::env::var_os("ROTORSCOPE_DEV_KEY").map(|v| v.into_string().unwrap_or_default());
    if let Err(msg) = devkey::check(key.as_deref()) {
        panic!("hub-p4: {msg}");
    }
}
