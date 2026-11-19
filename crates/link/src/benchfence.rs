//! Build-script helper shared by every firmware's `build.rs` (included with
//! `#[path = ".../crates/link/src/benchfence.rs"] mod benchfence;`): a build
//! that carries a bench-only feature or bench-only build switch must say so
//! on purpose.
//!
//! Bench features fake data (made-up drones, made-up GPS), open debug doors
//! (console commands that panic, inject samples, un-redact serials) or loop
//! the board's own pins back. Nothing in the image itself tells a field build
//! from such a build, so the build refuses to compile one unless the
//! environment has `ROTORSCOPE_BENCH=1` (tools/bench, tools/flash-node.sh and
//! docs/BENCH.md set it), and hands the firmware the list to print in its
//! hello line (`ROTORSCOPE_BENCH_JSON`, a JSON array, `[]` for a normal
//! build), and the evidence boot event (`ROTORSCOPE_BENCH_LIST`, comma
//! separated, empty for a normal build). Every image, hub-p4 included,
//! also carries the tag `bench-build:<list>` (`ROTORSCOPE_BENCH_TAG`,
//! `bench-build:none` for a normal build), so tools/flash-node.sh and
//! tools/dev-board.sh can refuse to flash a bench image without
//! `ROTORSCOPE_BENCH=1` by reading the ELF. On hub-p4 it sits in
//! `BUILD_TAGS` (`firmware/hub-p4/src/main.rs`) beside the source commit.
//!
//! Not part of the `link` library (it uses `std`); compiled in `link`'s unit
//! tests only, which test the decision ([`decide`]).

/// The environment variable that allows a bench build.
pub const ALLOW: &str = "ROTORSCOPE_BENCH";

/// What a build may carry: `Ok(list)` (empty for a normal build), or the
/// error message the build stops with.
///
/// `firmware`: the package name, for the message. `features`: the bench
/// features compiled in. `switches`: the bench build switches (environment
/// variables read with `option_env!`) that are set. `allow`: the value of
/// `ROTORSCOPE_BENCH`.
pub fn decide(firmware: &str, features: &[&str], switches: &[&str], allow: Option<&str>) -> Result<std::vec::Vec<std::string::String>, std::string::String> {
    let mut on: std::vec::Vec<std::string::String> = features.iter().map(|f| (*f).into()).collect();
    on.extend(switches.iter().map(|s| std::format!("env:{s}")));
    if on.is_empty() || allow.map(str::trim) == Some("1") {
        return Ok(on);
    }
    Err(std::format!(
        "{firmware}: bench-only build ({}) without ROTORSCOPE_BENCH=1.\n\
         These fake data or open debug doors and must never reach a fielded unit.\n\
         For a bench build on purpose: ROTORSCOPE_BENCH=1 cargo ... (docs/BENCH.md \"Bench builds\").\n\
         For a field build: drop the feature(s) / unset the variable(s).",
        on.join(", ")
    ))
}

/// `["a","b"]`, `[]` when empty. Names are cargo feature or variable names
/// (no quotes or backslashes).
pub fn json(list: &[std::string::String]) -> std::string::String {
    let items: std::vec::Vec<std::string::String> = list.iter().map(|s| std::format!("\"{s}\"")).collect();
    std::format!("[{}]", items.join(","))
}

/// The ELF tag: `bench-build:a,b`, `bench-build:none` when empty.
pub fn tag(list: &[std::string::String]) -> std::string::String {
    if list.is_empty() { "bench-build:none".into() } else { std::format!("bench-build:{}", list.join(",")) }
}

/// `CARGO_FEATURE_<NAME>` as cargo spells it for a feature name.
pub fn feature_var(feature: &str) -> std::string::String {
    std::format!("CARGO_FEATURE_{}", feature.to_ascii_uppercase().replace('-', "_"))
}

/// The whole build-script step: read the environment, stop the build or
/// pass the list on. `features`: every bench feature this firmware has;
/// `switches`: every bench build switch it reads.
pub fn run(firmware: &str, features: &[&str], switches: &[&str]) {
    std::println!("cargo:rerun-if-env-changed={ALLOW}");
    for s in switches {
        std::println!("cargo:rerun-if-env-changed={s}");
    }
    let on_f: std::vec::Vec<&str> = features.iter().copied().filter(|f| std::env::var_os(feature_var(f)).is_some()).collect();
    let on_s: std::vec::Vec<&str> = switches.iter().copied().filter(|s| std::env::var_os(s).is_some()).collect();
    let allow = std::env::var(ALLOW).ok();
    match decide(firmware, &on_f, &on_s, allow.as_deref()) {
        Ok(list) => {
            if !list.is_empty() {
                std::println!("cargo:warning={firmware}: BENCH BUILD ({}): never flash it into a fielded unit", list.join(", "));
            }
            std::println!("cargo:rustc-env=ROTORSCOPE_BENCH_LIST={}", list.join(","));
            std::println!("cargo:rustc-env=ROTORSCOPE_BENCH_JSON={}", json(&list));
            std::println!("cargo:rustc-env=ROTORSCOPE_BENCH_TAG={}", tag(&list));
        }
        Err(msg) => panic!("{msg}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_build_needs_nothing() {
        assert_eq!(decide("x", &[], &[], None), Ok(vec![]));
        assert_eq!(decide("x", &[], &[], Some("0")), Ok(vec![]));
        assert_eq!(json(&[]), "[]");
        assert_eq!(tag(&[]), "bench-build:none");
    }

    #[test]
    fn bench_feature_needs_the_switch() {
        let e = decide("hub-p4", &["fakedrone"], &[], None).unwrap_err();
        assert!(e.contains("hub-p4") && e.contains("fakedrone") && e.contains("ROTORSCOPE_BENCH=1"), "{e}");
        assert!(decide("hub-p4", &["fakedrone"], &[], Some("0")).is_err());
        assert!(decide("hub-p4", &["fakedrone"], &[], Some("yes")).is_err());
        assert!(decide("hub-p4", &["fakedrone"], &[], Some("")).is_err());
        let ok = decide("hub-p4", &["fakedrone", "faulttest"], &[], Some("1")).unwrap();
        assert_eq!(ok, vec!["fakedrone".to_string(), "faulttest".to_string()]);
        assert_eq!(json(&ok), r#"["fakedrone","faulttest"]"#);
        assert_eq!(tag(&ok), "bench-build:fakedrone,faulttest");
    }

    #[test]
    fn bench_switch_counts_too() {
        assert!(decide("node-ble", &[], &["ROTORSCOPE_BENCH_CAN"], None).is_err());
        let ok = decide("node-ble", &[], &["ROTORSCOPE_BENCH_CAN"], Some("1")).unwrap();
        assert_eq!(ok, vec!["env:ROTORSCOPE_BENCH_CAN".to_string()]);
    }

    #[test]
    fn feature_names_as_cargo_spells_them() {
        assert_eq!(feature_var("gpsloop-m10"), "CARGO_FEATURE_GPSLOOP_M10");
        assert_eq!(feature_var("faulttest"), "CARGO_FEATURE_FAULTTEST");
    }
}
