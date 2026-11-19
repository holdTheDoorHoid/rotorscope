//! What the checker reports about continuity (WP-14a): the previous boot's
//! unsigned tail vouched for by the next boot's signature, and the newest
//! boot named in the NOT PROVEN line.

mod common;

use common::unit::Unit;
use evidence::verify::{self, Input, Options};

#[test]
fn the_next_boot_vouches_for_the_unsigned_tail() {
    let mut u = Unit::new();
    let a = u.boot(1, 6, 2, "first");
    let b = u.boot(2, 6, 0, "second");
    let opts = Options { allow_test_key: true, ..Options::default() };
    // Alone, boot 1's last two notes are not signed: a gap.
    let r = verify::verify(&[Input::new("boot1.bin", a.bytes.clone())], &opts);
    assert_eq!(r.exit_code(), 3, "{:?}", r.findings);
    // With boot 2, whose signed continuity record names their head: covered.
    let r = verify::verify(&[Input::new("boot1.bin", a.bytes), Input::new("boot2.bin", b.bytes)], &opts);
    assert_eq!(r.exit_code(), 0, "{:?}", r.findings);
    let c = r.chains.iter().find(|c| c.boot == [1; 8]).unwrap();
    assert_eq!(c.vouched_by_next, c.last_seq);
    assert_eq!(c.signed_through, c.last_seq);
    let k = r.chains.iter().find(|c| c.boot == [2; 8]).unwrap().continuity.unwrap();
    assert_eq!((k.counter, k.prev_boot), (2, [1; 8]));
    assert!(r.not_proven().contains("boot counter 2"), "{}", r.not_proven());
    assert!(r.not_proven().starts_with(verify::NOT_PROVEN));
}

#[test]
fn logs_without_continuity_keep_the_plain_not_proven_line() {
    let r = verify::verify(&[], &Options::default());
    assert_eq!(r.not_proven(), verify::NOT_PROVEN);
}
