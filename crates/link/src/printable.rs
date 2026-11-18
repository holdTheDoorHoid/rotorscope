//! Text that arrived over the air (an SSID, a serial number, a callsign, a
//! firmware string in a Hello), made safe to print on a console or write
//! into evidence text. Nodes clean what they send, but the hub must not rely
//! on that: a forged or broken frame can carry any bytes, and a terminal
//! escape sequence or a newline in a console line or a log is an injection.
//!
//! [`Printable`] shows every character outside printable ASCII (space to
//! `~`) as `?`, one `?` per character. [`JsonPrintable`] also replaces `"`
//! and `\`, for text placed between quotes in a JSON line.

use core::fmt;

/// See the module docs.
pub struct Printable<'a>(pub &'a str);

/// As [`Printable`], also replacing `"` and `\` (inside a JSON string).
pub struct JsonPrintable<'a>(pub &'a str);

/// The character to show for `c`.
pub fn clean_char(c: char, json: bool) -> char {
    match c {
        '"' | '\\' if json => '?',
        ' '..='~' => c,
        _ => '?',
    }
}

fn write(f: &mut fmt::Formatter<'_>, s: &str, json: bool) -> fmt::Result {
    // Runs of clean characters go out as one slice.
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if clean_char(c, json) != c {
            f.write_str(&s[start..i])?;
            f.write_str("?")?;
            start = i + c.len_utf8();
        }
    }
    f.write_str(&s[start..])
}

impl fmt::Display for Printable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write(f, self.0, false)
    }
}

impl fmt::Display for JsonPrintable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write(f, self.0, true)
    }
}

/// True if `s` needs no cleaning.
pub fn is_printable(s: &str) -> bool {
    s.chars().all(|c| clean_char(c, false) == c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(format!("{}", Printable("DJI-Mavic 3 ~#{}")), "DJI-Mavic 3 ~#{}");
        assert!(is_printable("RSTEST-SD-0001"));
        assert_eq!(format!("{}", Printable("")), "");
    }

    #[test]
    fn controls_and_escapes_become_question_marks() {
        assert_eq!(format!("{}", Printable("a\nb\rc\td")), "a?b?c?d");
        assert_eq!(format!("{}", Printable("\x1b[2J\x1b]0;x\x07")), "?[2J?]0;x?");
        assert_eq!(format!("{}", Printable("x\0y\x7f")), "x?y?");
        assert!(!is_printable("ok\n"));
    }

    #[test]
    fn non_ascii_is_one_question_mark_per_character() {
        // Bidirectional override, a two-byte and a four-byte character.
        assert_eq!(format!("{}", Printable("a\u{202e}b")), "a?b");
        assert_eq!(format!("{}", Printable("caf\u{e9}")), "caf?");
        assert_eq!(format!("{}", Printable("\u{1f681}!")), "?!");
    }

    #[test]
    fn json_variant_also_replaces_quote_and_backslash() {
        assert_eq!(format!("{}", JsonPrintable(r#"gw "x"\y"#)), "gw ?x??y");
        assert_eq!(format!("{}", Printable(r#"gw "x"\y"#)), r#"gw "x"\y"#);
        assert_eq!(format!("{}", JsonPrintable("gw OPEN no key")), "gw OPEN no key");
    }
}
