//! The hub's build identity (used by `firmware/hub-p4/build.rs` through
//! `#[path]`; compiled into the library only for its unit tests): the
//! source commit and whether the tree was modified, as the one string the
//! build hands the firmware in `ROTORSCOPE_GIT`. Never fails a build: outside
//! a git checkout (a source archive) the answer is `unknown`.
//!
//! `<40 hex>` a clean checkout of that commit; `<40 hex>-dirty` that commit
//! with changes to tracked files (what ran is not exactly that commit);
//! `unknown` no git, or git could not say. The firmware parses it back with
//! `evidence::format::boot::parse_git` for the boot event.

/// `commit`: `git rev-parse HEAD` output; `status`: `git status --porcelain
/// --untracked-files=no` output (`None` if it could not run).
pub fn git_field(commit: Option<&str>, status: Option<&str>) -> String {
    let Some(c) = commit.map(str::trim).filter(|c| c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit())) else {
        return "unknown".into();
    };
    let c = c.to_ascii_lowercase();
    match status {
        Some(s) if s.trim().is_empty() => c,
        Some(_) => format!("{c}-dirty"),
        // The commit is known but not whether the tree matches it.
        None => format!("{c}-unknown"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const C: &str = "6651ecd0123456789abcdef0123456789abcdef0";

    #[test]
    fn clean_dirty_unknown() {
        assert_eq!(git_field(Some(C), Some("")), C);
        assert_eq!(git_field(Some(&format!("{C}\n")), Some("\n")), C);
        assert_eq!(git_field(Some(C), Some(" M firmware/hub-p4/src/main.rs\n")), format!("{C}-dirty"));
        assert_eq!(git_field(Some(C), None), format!("{C}-unknown"));
        assert_eq!(git_field(None, None), "unknown");
        assert_eq!(git_field(Some("fatal: not a git repository"), Some("")), "unknown");
        assert_eq!(git_field(Some(&C.to_ascii_uppercase()), Some("")), C);
    }

    #[test]
    fn the_firmware_reads_it_back() {
        use crate::format::boot::{self, parse_git};
        let mut want = [0u8; 20];
        for (i, b) in want.iter_mut().enumerate() {
            *b = u8::from_str_radix(&C[2 * i..2 * i + 2], 16).unwrap();
        }
        assert_eq!(parse_git(&git_field(Some(C), Some(""))), (want, boot::GIT_CLEAN));
        assert_eq!(parse_git(&git_field(Some(C), Some("x"))), (want, boot::GIT_DIRTY));
        assert_eq!(parse_git(&git_field(Some(C), None)), (want, boot::GIT_UNKNOWN));
        assert_eq!(parse_git("unknown"), ([0; 20], boot::GIT_UNKNOWN));
        assert_eq!(parse_git(""), ([0; 20], boot::GIT_UNKNOWN));
    }
}
