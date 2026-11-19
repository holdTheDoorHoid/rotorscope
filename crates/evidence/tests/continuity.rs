//! Continuity across boots (WP-14a, HB-3): a boot left out, or the end of a
//! boot's log cut off at a checkpoint, used to pass as VERIFIED.

mod common;

use common::unit::Unit;
use evidence::verify::{self, Input, Level, Options};

fn opts() -> Options {
    Options { allow_test_key: true, ..Options::default() }
}

fn fails(r: &verify::Report) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == Level::Fail).map(|f| format!("{}: {}", f.at, f.what)).collect()
}

fn notes(r: &verify::Report) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == Level::Note).map(|f| f.what.clone()).collect()
}

fn three() -> (Unit, [common::unit::Boot; 3]) {
    let mut u = Unit::new();
    let a = u.boot(1, 6, 2, "first boot");
    let b = u.boot(2, 6, 2, "second boot");
    let c = u.boot(3, 6, 0, "third boot");
    (u, [a, b, c])
}

#[test]
fn consecutive_boots_verify() {
    let (_, [a, b, c]) = three();
    let r = verify::verify(
        &[Input::new("boot1.bin", a.bytes), Input::new("boot2.bin", b.bytes), Input::new("boot3.bin", c.bytes)],
        &opts(),
    );
    assert!(fails(&r).is_empty(), "{:?}", r.findings);
    assert_eq!(r.exit_code(), 0, "{:?}", r.findings);
}

#[test]
fn a_boot_left_out_between_two_fails() {
    let (_, [a, _b, c]) = three();
    let r = verify::verify(&[Input::new("boot1.bin", a.bytes), Input::new("boot3.bin", c.bytes)], &opts());
    let f = fails(&r);
    assert!(f.iter().any(|f| f.contains("boot counter 2") && f.contains("not supplied")), "{f:?}");
    assert_eq!(r.exit_code(), 1);
}

#[test]
fn a_boot_left_out_is_a_gap_with_allow_gaps() {
    let (_, [a, _b, c]) = three();
    let r = verify::verify(&[Input::new("boot1.bin", a.bytes), Input::new("boot3.bin", c.bytes)], &Options { gaps_allowed: true, ..opts() });
    assert!(fails(&r).is_empty(), "{:?}", r.findings);
    assert_eq!(r.exit_code(), 3);
}

#[test]
fn the_end_of_a_boot_cut_at_a_checkpoint_fails() {
    let (_, [a, b, _c]) = three();
    // Boot 1's records: #0 boot, #1 continuity, then notes with a checkpoint
    // after every third. Cut right after the first checkpoint (#5): every
    // remaining record is signed, so this used to verify.
    let cut = a.through(5);
    let r = verify::verify(&[Input::new("boot1.bin", cut.clone())], &opts());
    assert_eq!(r.exit_code(), 0, "alone, the cut boot still checks: {:?}", r.findings);
    let r = verify::verify(&[Input::new("boot1.bin", cut), Input::new("boot2.bin", b.bytes)], &opts());
    let f = fails(&r);
    assert!(f.iter().any(|f| f.contains("ends at record #5") && f.contains("cut off")), "{f:?}");
}

#[test]
fn an_older_boot_rewritten_after_the_next_began_fails() {
    // Same key, same counter store history, a different last note: the
    // next boot named the real one.
    let mut u = Unit::new();
    let a = u.boot(1, 6, 2, "what happened");
    let b = u.boot(2, 3, 0, "next boot");
    let mut forger = Unit::new();
    let a2 = forger.boot(1, 6, 2, "what is claim");
    assert_eq!(a.bytes.len(), a2.bytes.len());
    let r = verify::verify(&[Input::new("boot1.bin", a2.bytes), Input::new("boot2.bin", b.bytes)], &opts());
    let f = fails(&r);
    assert!(f.iter().any(|f| f.contains("is not the one the unit had stored")), "{f:?}");
}

#[test]
fn the_earliest_boot_supplied_names_what_came_before() {
    let (_, [_a, b, _c]) = three();
    let r = verify::verify(&[Input::new("boot2.bin", b.bytes)], &opts());
    assert!(fails(&r).is_empty(), "{:?}", r.findings);
    assert!(notes(&r).iter().any(|n| n.contains("boot counter 2") && n.contains("outside what was checked")), "{:?}", notes(&r));
}

#[test]
fn a_wiped_card_restarts_the_counter_and_says_so() {
    let mut u = Unit::new();
    u.boot(1, 3, 0, "x");
    u.store.slot = [None, None];
    let b = u.boot(2, 3, 0, "y");
    let r = verify::verify(&[Input::new("boot2.bin", b.bytes)], &opts());
    assert!(fails(&r).is_empty());
    assert!(notes(&r).iter().any(|n| n.contains("boot counter 1") && n.contains("held no counter")), "{:?}", notes(&r));
}
