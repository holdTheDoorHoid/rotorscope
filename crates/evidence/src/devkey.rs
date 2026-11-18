//! The hub's build-time development key check (used by
//! `firmware/hub-p4/build.rs` through `#[path]`; compiled into the library
//! only for its unit tests).
//!
//! `ROTORSCOPE_DEV_KEY` unset: the hub signs with the published test key and
//! says so on its screen and in every checkpoint. Set: it must be a usable
//! key, exactly 64 hex digits and not zero; anything else stops the build,
//! because the firmware used to fall back to the published test key without
//! a word. The value is never printed, not even its length.

/// `Ok(false)`: no dev key (test key build). `Ok(true)`: a well-formed dev
/// key. `Err`: the message the build stops with.
pub fn check(value: Option<&str>) -> Result<bool, &'static str> {
    let Some(v) = value else { return Ok(false) };
    let b = v.as_bytes();
    if b.len() != 64 || !b.iter().all(u8::is_ascii_hexdigit) {
        return Err("ROTORSCOPE_DEV_KEY is set but is not exactly 64 hex digits (no spaces, no 0x). \
                    Unset it to build with the published TEST key (the screen then says so), \
                    or set a real development key (docs/EVIDENCE.md). The value is not shown.");
    }
    if b.iter().all(|&c| c == b'0') {
        return Err("ROTORSCOPE_DEV_KEY is zero, which is not a P-256 private key. The value is not shown.");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_is_the_test_key() {
        assert_eq!(check(None), Ok(false));
    }

    #[test]
    fn well_formed_key() {
        assert_eq!(check(Some(&"a1".repeat(32))), Ok(true));
        assert_eq!(check(Some(&"ABcd".repeat(16))), Ok(true));
    }

    #[test]
    fn malformed_keys_stop_the_build() {
        for bad in [
            String::new(),
            "a1".repeat(31),
            "a1".repeat(33),
            format!("0x{}", "a1".repeat(31)),
            format!("{} ", "a1".repeat(32)),
            "g1".repeat(32),
            "0".repeat(64),
        ] {
            let e = check(Some(&bad)).unwrap_err();
            assert!(e.contains("ROTORSCOPE_DEV_KEY") && e.contains("not shown"), "{e}");
            assert!(!e.contains(&bad) || bad.is_empty());
        }
    }
}
