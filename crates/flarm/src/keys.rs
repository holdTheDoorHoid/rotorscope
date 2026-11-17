//! The FLARM legacy key constants, loaded at run time.
//!
//! **No key is in this repository.** The owner may use them (statement of
//! 2026-11-17) but the repository is public and that permission does not
//! cover publishing them. They come from a small text file: on the hub,
//! `ROTORSCP/FLARMKEY.TXT` on the SD card; on a laptop, `tools/flarm.keys`
//! as written by `cargo run -p flarm-keys` from a SoftRF checkout (both
//! names are in `.gitignore`). The file and the parsed [`Keys`] must never
//! be printed, logged, or sent between boards: [`Keys`] has no `Display`,
//! its `Debug` prints nothing of the values, and [`KeyError`] names lines,
//! never their contents.
//!
//! # Format
//!
//! UTF-8 or ASCII text, one `name = value` per line; `#` starts a comment;
//! blank lines are ignored. Values are 32-bit words in hexadecimal (an
//! optional `0x`), separated by spaces and/or commas. Every name must
//! appear exactly once:
//!
//! ```text
//! # FLARM legacy keys. Never commit, print, log or relay this file.
//! key1 = w0 w1 w2 w3 w4 w5 w6 w7   # v6 key table (8 words)
//! key2 = w                          # v6 key mixing multiplier
//! key3 = w                          # v6 key final XOR
//! key4 = w                          # v7 key schedule word
//! key5 = w0 w1 w2 w3                # v7 XXTEA key (4 words)
//! ```
//!
//! The names follow SoftRF's `LEGACY_KEY1` .. `LEGACY_KEY5` in `Legacy.h`.

/// The constants the legacy decoder needs. Parse with [`Keys::parse`].
#[derive(Clone)]
pub struct Keys {
    pub(crate) table: [u32; 8],
    pub(crate) mix: u32,
    pub(crate) xor: u32,
    pub(crate) v7_word: u32,
    pub(crate) v7_xxtea: [u32; 4],
}

impl core::fmt::Debug for Keys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Keys(<redacted>)")
    }
}

/// Why a key file was refused. Carries line numbers and names only, never
/// any of the file's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// Line `n` (1-based) has no `=`.
    Syntax(usize),
    /// Line `n` names something other than `key1`..`key5`.
    UnknownName(usize),
    /// Line `n` repeats a name.
    Duplicate(usize),
    /// Line `n` has a value that is not a 32-bit hex word.
    BadWord(usize),
    /// Line `n` has the wrong number of words for its name.
    WordCount(usize),
    /// This name never appeared.
    Missing(&'static str),
}

const NAMES: [(&str, usize); 5] = [
    ("key1", 8),
    ("key2", 1),
    ("key3", 1),
    ("key4", 1),
    ("key5", 4),
];

impl Keys {
    /// Parse a key file (see the module docs for the format).
    pub fn parse(text: &str) -> Result<Keys, KeyError> {
        let mut words = [[0u32; 8]; 5];
        let mut seen = [false; 5];
        for (i, raw) in text.lines().enumerate() {
            let line_no = i + 1;
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (name, value) = line.split_once('=').ok_or(KeyError::Syntax(line_no))?;
            let name = name.trim();
            let k = NAMES
                .iter()
                .position(|(n, _)| n.eq_ignore_ascii_case(name))
                .ok_or(KeyError::UnknownName(line_no))?;
            if seen[k] {
                return Err(KeyError::Duplicate(line_no));
            }
            seen[k] = true;
            let mut count = 0usize;
            for tok in value.split([' ', '\t', ',']).filter(|t| !t.is_empty()) {
                let hex = tok
                    .strip_prefix("0x")
                    .or_else(|| tok.strip_prefix("0X"))
                    .unwrap_or(tok);
                if hex.is_empty() || hex.len() > 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(KeyError::BadWord(line_no));
                }
                let w = u32::from_str_radix(hex, 16).map_err(|_| KeyError::BadWord(line_no))?;
                if count >= NAMES[k].1 {
                    return Err(KeyError::WordCount(line_no));
                }
                words[k][count] = w;
                count += 1;
            }
            if count != NAMES[k].1 {
                return Err(KeyError::WordCount(line_no));
            }
        }
        if let Some(k) = seen.iter().position(|s| !s) {
            return Err(KeyError::Missing(NAMES[k].0));
        }
        Ok(Keys {
            table: words[0],
            mix: words[1][0],
            xor: words[2][0],
            v7_word: words[3][0],
            v7_xxtea: [words[4][0], words[4][1], words[4][2], words[4][3]],
        })
    }
}
