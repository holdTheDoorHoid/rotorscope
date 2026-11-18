//! Evidence file names on the SD card: `EVnnnnnn.EVL`, one new file per
//! mount, numbered one past the highest already there, never reused and
//! never rewritten. Pure logic (no card access), so it is tested here; the
//! hub's `sd.rs` feeds it the directory listing.
//!
//! The card is not trusted: someone may have put anything in the folder. Only
//! a regular file with exactly our name pattern and a number in
//! `1..=`[`MAX_NO`] counts. A directory or volume label with such a name, a
//! name with other characters, or number 0 is ignored. When the highest
//! number is taken (for example a planted `EV999999.EVL`), there is no "one
//! past" left: [`choose_next`] then takes the lowest number that is free,
//! deliberately, instead of failing every mount (which would stop the
//! evidence log while the CSV log, and the screen, carried on).

/// Highest file number (six digits).
pub const MAX_NO: u32 = 999_999;

/// Numbers checked per pass of the directory when looking for a free one.
pub const WINDOW: u32 = 1024;

/// The number of an evidence file, if this directory entry is one: a
/// regular file (`is_file`: not a directory, not a volume label) whose 8.3
/// name is `EV` + six digits, extension `EVL`, number 1 to [`MAX_NO`].
/// `base` and `ext` as stored in the directory entry (upper case, no
/// padding).
pub fn file_no(base: &[u8], ext: &[u8], is_file: bool) -> Option<u32> {
    if !is_file || ext != b"EVL" || base.len() != 8 || !base.starts_with(b"EV") {
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
    /// The usual case: one past the highest file there. `prev` is that
    /// file's number (0 = the folder has none), whose last record the new
    /// file names in its first event.
    After { prev: u32, no: u32 },
    /// [`MAX_NO`] is taken: the lowest free number. The files no longer
    /// sort in the order they were written, and there is no "previous"
    /// file to link to.
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
/// calling its argument with the number of every evidence file (filtered
/// with [`file_no`]); it is called once in the usual case and a few more
/// times only when [`MAX_NO`] is taken (one pass per [`WINDOW`] numbers,
/// with a small bitmap, so no listing is ever held in memory). `Ok(None)`
/// only if every number up to [`MAX_NO`] is taken, which a FAT folder
/// (at most 65,536 entries) cannot hold.
pub fn choose_next<E>(mut scan: impl FnMut(&mut dyn FnMut(u32)) -> Result<(), E>) -> Result<Option<Next>, E> {
    let mut highest = 0u32;
    let mut count = 0u32;
    scan(&mut |n| {
        if (1..=MAX_NO).contains(&n) {
            highest = highest.max(n);
            count += 1;
        }
    })?;
    if highest < MAX_NO {
        return Ok(Some(Next::After { prev: highest, no: highest + 1 }));
    }
    // The lowest free number is at most count + 1 (count numbers are taken).
    let last = MAX_NO.min(count.saturating_add(1));
    let mut start = 1u32;
    while start <= last {
        let mut taken = [0u64; (WINDOW / 64) as usize];
        scan(&mut |n| {
            if n >= start && n - start < WINDOW {
                let i = n - start;
                taken[(i / 64) as usize] |= 1 << (i % 64);
            }
        })?;
        for i in 0..WINDOW {
            let no = start + i;
            if no > MAX_NO {
                break;
            }
            if taken[(i / 64) as usize] & (1 << (i % 64)) == 0 {
                return Ok(Some(Next::Lowest { no }));
            }
        }
        start += WINDOW;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (base, ext, is_file) entries, as a card listing.
    fn listing<'a>(entries: &'a [(&'a str, &'a str, bool)]) -> impl FnMut(&mut dyn FnMut(u32)) -> Result<(), ()> + 'a {
        move |f: &mut dyn FnMut(u32)| {
            for (b, e, file) in entries {
                if let Some(n) = file_no(b.as_bytes(), e.as_bytes(), *file) {
                    f(n);
                }
            }
            Ok(())
        }
    }

    fn next(entries: &[(&str, &str, bool)]) -> Option<Next> {
        choose_next(listing(entries)).unwrap()
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
    fn which_entries_count() {
        assert_eq!(file_no(b"EV000001", b"EVL", true), Some(1));
        assert_eq!(file_no(b"EV999999", b"EVL", true), Some(999_999));
        // A directory or volume label with a file's name.
        assert_eq!(file_no(b"EV000005", b"EVL", false), None);
        // Number 0, wrong lengths, other characters, other extensions.
        assert_eq!(file_no(b"EV000000", b"EVL", true), None);
        assert_eq!(file_no(b"EV00001", b"EVL", true), None);
        assert_eq!(file_no(b"EV0000001", b"EVL", true), None);
        assert_eq!(file_no(b"EV00000A", b"EVL", true), None);
        assert_eq!(file_no(b"EV-00001", b"EVL", true), None);
        assert_eq!(file_no(b"EV 00001", b"EVL", true), None);
        assert_eq!(file_no(b"XV000001", b"EVL", true), None);
        assert_eq!(file_no(b"EV000001", b"CSV", true), None);
        assert_eq!(file_no(b"EV000001", b"", true), None);
    }

    #[test]
    fn empty_folder_starts_at_one() {
        assert_eq!(next(&[]), Some(Next::After { prev: 0, no: 1 }));
        assert_eq!(next(&[("README", "TXT", true)]), Some(Next::After { prev: 0, no: 1 }));
    }

    #[test]
    fn one_past_the_highest() {
        let e = [("EV000001", "EVL", true), ("EV000007", "EVL", true), ("EV000003", "EVL", true)];
        assert_eq!(next(&e), Some(Next::After { prev: 7, no: 8 }));
        assert_eq!(next(&e).unwrap().no(), 8);
    }

    #[test]
    fn planted_directories_and_junk_are_ignored() {
        let e = [
            ("EV000002", "EVL", true),
            // A directory named like a later file (it would have been
            // "previous" and failed to open as a file, every mount).
            ("EV000500", "EVL", false),
            ("EV00050X", "EVL", true),
            ("EV000000", "EVL", true),
        ];
        assert_eq!(next(&e), Some(Next::After { prev: 2, no: 3 }));
    }

    #[test]
    fn planted_highest_number_takes_the_lowest_free() {
        // EV999999.EVL used to make the next name EV1000000.EVL: an
        // invalid 8.3 name, so every mount failed and evidence stopped.
        let e = [("EV000001", "EVL", true), ("EV000002", "EVL", true), ("EV999999", "EVL", true)];
        assert_eq!(next(&e), Some(Next::Lowest { no: 3 }));
        assert_eq!(next(&[("EV999999", "EVL", true)]), Some(Next::Lowest { no: 1 }));
        // A planted directory with that name is just ignored.
        assert_eq!(next(&[("EV000004", "EVL", true), ("EV999999", "EVL", false)]), Some(Next::After { prev: 4, no: 5 }));
        // The one below the top is still an ordinary step.
        assert_eq!(next(&[("EV999998", "EVL", true)]), Some(Next::After { prev: 999_998, no: 999_999 }));
    }

    #[test]
    fn lowest_free_beyond_the_first_window() {
        // 1..=2500 taken, plus the top: the answer is in the third window.
        let mut names: Vec<String> = (1..=2500).map(|n| format!("EV{n:06}")).collect();
        names.push("EV999999".into());
        let entries: Vec<(&str, &str, bool)> = names.iter().map(|n| (n.as_str(), "EVL", true)).collect();
        let mut passes = 0;
        let r = choose_next(|f: &mut dyn FnMut(u32)| {
            passes += 1;
            for (b, e, file) in &entries {
                if let Some(n) = file_no(b.as_bytes(), e.as_bytes(), *file) {
                    f(n);
                }
            }
            Ok::<(), ()>(())
        });
        assert_eq!(r, Ok(Some(Next::Lowest { no: 2501 })));
        // One pass for the highest, then one per window up to 2501.
        assert_eq!(passes, 1 + 3);
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
