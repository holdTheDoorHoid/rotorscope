//! Evidence file names on the SD card: `EVnnnnnn.EVL`, one new file per
//! mount, numbered one past the highest already there, never rewritten
//! (a name that exists is never taken; the number of a deleted file can
//! come back). Pure logic (no card access), so it is tested here; the
//! hub's `sd.rs` feeds it the directory listing.
//!
//! The card is not trusted: someone may have put anything in the folder.
//! Only a **regular file** with exactly our name pattern and a number in
//! `1..=`[`MAX_NO`] counts as an evidence file (for "the highest" and for
//! the previous file the new one links to). A directory or volume label with
//! such a name counts for nothing, except that its name is taken: the new
//! file never gets a number whose name exists in any form. Names with other
//! characters, and number 0, are ignored. When no number above the highest
//! file is free (a planted `EV999999.EVL`), [`choose_next`] takes the lowest
//! free number instead, deliberately, rather than fail every mount (which
//! would stop the evidence log while the CSV log, and the screen, carried
//! on).

/// Highest file number (six digits).
pub const MAX_NO: u32 = 999_999;

/// Numbers checked per pass of the directory when looking for a free one.
pub const WINDOW: u32 = 1024;

/// The number in a directory entry's 8.3 name if it has our pattern: `EV` +
/// six digits, extension `EVL`, number 1 to [`MAX_NO`]. `base` and `ext` as
/// stored in the directory entry (upper case, no padding). Whether the
/// entry is a regular file is the caller's to say (see [`choose_next`]).
pub fn number(base: &[u8], ext: &[u8]) -> Option<u32> {
    if ext != b"EVL" || base.len() != 8 || !base.starts_with(b"EV") {
        return None;
    }
    let mut n = 0u32;
    for &d in &base[2..] {
        if !d.is_ascii_digit() {
            return None;
        }
        n = n * 10 + u32::from(d - b'0');
    }
    (1..=MAX_NO).contains(&n).then_some(n)
}

/// `EVnnnnnn.EVL` for `no` in `1..=MAX_NO`.
pub fn name(no: u32) -> Option<[u8; 12]> {
    if !(1..=MAX_NO).contains(&no) {
        return None;
    }
    let mut out = *b"EV000000.EVL";
    let mut v = no;
    for i in (2..8).rev() {
        out[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    Some(out)
}

/// The next file to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// The usual case: above the highest evidence file there (one past it,
    /// unless that name is taken by something else). `prev` is that file's
    /// number (0 = the folder has none), whose last record the new file
    /// names in its first event.
    After { prev: u32, no: u32 },
    /// No number above the highest file is free: the lowest free number.
    /// The files no longer sort in the order they were written, and there
    /// is no "previous" file to link to.
    Lowest { no: u32 },
}

impl Next {
    pub fn no(self) -> u32 {
        match self {
            Next::After { no, .. } | Next::Lowest { no } => no,
        }
    }
}

/// Choose the next file number. `scan` lists the folder once per call,
/// calling its argument with `(number, is_file)` for every entry whose name
/// has our pattern ([`number`]; `is_file`: a regular file, not a directory
/// or volume label). It is called once to find the highest file, and once
/// per [`WINDOW`] numbers searched for a free name (one pass in the usual
/// case; a small bitmap, so no listing is ever held in memory). `Ok(None)`
/// only if every number up to [`MAX_NO`] is taken, which a FAT folder (at
/// most 65,536 entries) cannot hold.
pub fn choose_next<E>(mut scan: impl FnMut(&mut dyn FnMut(u32, bool)) -> Result<(), E>) -> Result<Option<Next>, E> {
    let mut highest = 0u32;
    let mut count = 0u32;
    scan(&mut |n, is_file| {
        if (1..=MAX_NO).contains(&n) {
            if is_file {
                highest = highest.max(n);
            }
            count = count.saturating_add(1);
        }
    })?;
    if highest < MAX_NO
        && let Some(no) = lowest_free(&mut scan, highest + 1, count)?
    {
        return Ok(Some(Next::After { prev: highest, no }));
    }
    Ok(lowest_free(&mut scan, 1, count)?.map(|no| Next::Lowest { no }))
}

/// The lowest number from `start` up to [`MAX_NO`] whose name no entry has.
/// At most `taken` entries exist, so one is free within `taken + 1`
/// numbers of `start` (if `MAX_NO` is not reached first).
fn lowest_free<E>(scan: &mut impl FnMut(&mut dyn FnMut(u32, bool)) -> Result<(), E>, start: u32, taken: u32) -> Result<Option<u32>, E> {
    let last = MAX_NO.min(start.saturating_add(taken));
    let mut from = start;
    while from <= last {
        let mut used = [0u64; (WINDOW / 64) as usize];
        scan(&mut |n, _| {
            if n >= from && n - from < WINDOW {
                let i = n - from;
                used[(i / 64) as usize] |= 1 << (i % 64);
            }
        })?;
        for i in 0..WINDOW {
            let no = from + i;
            if no > last {
                break;
            }
            if used[(i / 64) as usize] & (1 << (i % 64)) == 0 {
                return Ok(Some(no));
            }
        }
        from += WINDOW;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (base, ext, is_file) entries, as a card listing; the number of passes.
    fn run(entries: &[(&str, &str, bool)]) -> (Option<Next>, u32) {
        let mut passes = 0;
        let r = choose_next(|f: &mut dyn FnMut(u32, bool)| {
            passes += 1;
            for (b, e, file) in entries {
                if let Some(n) = number(b.as_bytes(), e.as_bytes()) {
                    f(n, *file);
                }
            }
            Ok::<(), ()>(())
        });
        (r.unwrap(), passes)
    }

    fn next(entries: &[(&str, &str, bool)]) -> Option<Next> {
        run(entries).0
    }

    #[test]
    fn names() {
        assert_eq!(&name(1).unwrap(), b"EV000001.EVL");
        assert_eq!(&name(999_999).unwrap(), b"EV999999.EVL");
        assert_eq!(&name(123_456).unwrap(), b"EV123456.EVL");
        assert_eq!(name(0), None);
        assert_eq!(name(1_000_000), None);
    }

    #[test]
    fn which_names_have_the_pattern() {
        assert_eq!(number(b"EV000001", b"EVL"), Some(1));
        assert_eq!(number(b"EV999999", b"EVL"), Some(999_999));
        // Number 0, wrong lengths, other characters, other extensions.
        assert_eq!(number(b"EV000000", b"EVL"), None);
        assert_eq!(number(b"EV00001", b"EVL"), None);
        assert_eq!(number(b"EV0000001", b"EVL"), None);
        assert_eq!(number(b"EV00000A", b"EVL"), None);
        assert_eq!(number(b"EV-00001", b"EVL"), None);
        assert_eq!(number(b"EV 00001", b"EVL"), None);
        assert_eq!(number(b"XV000001", b"EVL"), None);
        assert_eq!(number(b"EV000001", b"CSV"), None);
        assert_eq!(number(b"EV000001", b""), None);
    }

    #[test]
    fn empty_folder_starts_at_one() {
        assert_eq!(run(&[]), (Some(Next::After { prev: 0, no: 1 }), 2));
        assert_eq!(next(&[("README", "TXT", true)]), Some(Next::After { prev: 0, no: 1 }));
    }

    #[test]
    fn one_past_the_highest() {
        let e = [("EV000001", "EVL", true), ("EV000007", "EVL", true), ("EV000003", "EVL", true)];
        // Two passes: the highest, then the one window above it.
        assert_eq!(run(&e), (Some(Next::After { prev: 7, no: 8 }), 2));
        assert_eq!(next(&e).unwrap().no(), 8);
    }

    #[test]
    fn planted_directories_and_junk_do_not_count_as_files() {
        let e = [
            ("EV000002", "EVL", true),
            // A directory named like a later file (it used to become
            // "previous" and fail to open as a file, every mount).
            ("EV000500", "EVL", false),
            ("EV00050X", "EVL", true),
            ("EV000000", "EVL", true),
        ];
        assert_eq!(next(&e), Some(Next::After { prev: 2, no: 3 }));
    }

    #[test]
    fn a_directory_never_blocks_the_next_name() {
        // A directory holding the very name the next file needs: skip it
        // (creating the file would fail on every mount).
        let e = [("EV000004", "EVL", true), ("EV000005", "EVL", false), ("EV000006", "EVL", false)];
        assert_eq!(next(&e), Some(Next::After { prev: 4, no: 7 }));
        // The top name taken by a directory, above the highest file.
        let e = [("EV999998", "EVL", true), ("EV999999", "EVL", false)];
        assert_eq!(next(&e), Some(Next::Lowest { no: 1 }));
        let e = [("EV000004", "EVL", true), ("EV999999", "EVL", false)];
        assert_eq!(next(&e), Some(Next::After { prev: 4, no: 5 }));
    }

    #[test]
    fn planted_highest_number_takes_the_lowest_free() {
        // EV999999.EVL used to make the next name EV1000000.EVL: an
        // invalid 8.3 name, so every mount failed and evidence stopped.
        let e = [("EV000001", "EVL", true), ("EV000002", "EVL", true), ("EV999999", "EVL", true)];
        assert_eq!(next(&e), Some(Next::Lowest { no: 3 }));
        assert_eq!(next(&[("EV999999", "EVL", true)]), Some(Next::Lowest { no: 1 }));
        // The one below the top is still an ordinary step.
        assert_eq!(next(&[("EV999998", "EVL", true)]), Some(Next::After { prev: 999_998, no: 999_999 }));
    }

    #[test]
    fn lowest_free_beyond_the_first_window() {
        // 1..=2500 taken, plus the top: the answer is in the third window.
        let mut names: Vec<String> = (1..=2500).map(|n| format!("EV{n:06}")).collect();
        names.push("EV999999".into());
        let entries: Vec<(&str, &str, bool)> = names.iter().map(|n| (n.as_str(), "EVL", true)).collect();
        let (r, passes) = run(&entries);
        assert_eq!(r, Some(Next::Lowest { no: 2501 }));
        // One pass for the highest, then one per window up to 2501.
        assert_eq!(passes, 1 + 3);
    }

    #[test]
    fn many_directories_above_the_highest_file() {
        // 1500 directories right above the only file: the next name is past
        // them, found in two windows.
        let mut names: Vec<(String, bool)> = vec![("EV000010".into(), true)];
        names.extend((11..=1510).map(|n| (format!("EV{n:06}"), false)));
        let entries: Vec<(&str, &str, bool)> = names.iter().map(|(n, f)| (n.as_str(), "EVL", *f)).collect();
        let (r, passes) = run(&entries);
        assert_eq!(r, Some(Next::After { prev: 10, no: 1511 }));
        assert_eq!(passes, 1 + 2);
    }

    #[test]
    fn listing_errors_pass_through() {
        let r: Result<Option<Next>, &str> = choose_next(|_| Err("card gone"));
        assert_eq!(r, Err("card gone"));
    }

    #[test]
    fn duplicates_in_a_listing_are_harmless() {
        let e = [("EV999999", "EVL", true), ("EV999999", "EVL", true), ("EV000001", "EVL", true)];
        assert_eq!(next(&e), Some(Next::Lowest { no: 2 }));
    }
}
