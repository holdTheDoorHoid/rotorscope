//! Time plausibility (WP-14a, HB-11): what the checker can tell about a
//! log's UTC with no outside reference, from the hub's own uptime, the
//! firmware's build date and the order of boots.

mod common;

use evidence::format::{self, boot, CheckpointReason, EventCode, TimeSource};
use evidence::sign::SoftSigner;
use evidence::verify::{self, Input, Level, Options};
use evidence::writer::{Chain, Context};
use evidence::Signer;

/// 2026-11-01T00:00:00Z.
const BUILT_S: u64 = 1_793_491_200;
const BUILT_MS: u64 = BUILT_S * 1000;

enum Step {
    /// A note at this uptime with this UTC (None: no GPS time) and source.
    At(u64, Option<u64>, TimeSource),
    /// A clock event at this uptime, the record's own UTC as given.
    Clock(u64, Option<u64>),
}

fn log(id: u8, steps: &[Step]) -> Vec<u8> {
    let mut s = SoftSigner::public_test();
    let mut c = Chain::new([id; 8], [0xF1; 32]);
    let mut out = Vec::new();
    let mut b = [0u8; format::MAX_RECORD];
    let ctx = |t: u64, utc: Option<u64>, src: TimeSource| Context { uptime_ms: t, utc_ms: utc, time_source: src, time_age_ms: utc.map(|_| 100), ..Context::default() };
    let bd = boot::Boot {
        key_class: s.key_class() as u8,
        pubkey: s.public_key(),
        chip: (1, 3),
        ext: Some(boot::Ext { built_unix_s: BUILT_S, git: [1; 20], git_state: boot::GIT_CLEAN, counter_source: boot::COUNTER_NONE, boot_id_method: boot::BOOT_ID_MIXED }),
    };
    let mut data = [0u8; boot::LEN];
    let dn = bd.encode(&mut data);
    let n = c.event(&ctx(0, None, TimeSource::None), EventCode::Boot, b"hub-p4 test; bench: none", &data[..dn], &mut b).unwrap();
    out.extend_from_slice(&b[..n]);
    c.commit();
    let mut last_t = 0;
    for st in steps {
        let n = match *st {
            Step::At(t, u, src) => {
                last_t = t;
                c.event(&ctx(t, u, src), EventCode::Note, b"n", &[], &mut b).unwrap()
            }
            Step::Clock(t, u) => {
                last_t = t;
                c.event(&ctx(t, u, TimeSource::Gps), EventCode::Clock, b"UTC corrected", &[0; 25], &mut b).unwrap()
            }
        };
        out.extend_from_slice(&b[..n]);
        c.commit();
    }
    let n = c.checkpoint(&ctx(last_t + 1, None, TimeSource::None), CheckpointReason::Periodic, &mut s, &mut b).unwrap();
    out.extend_from_slice(&b[..n]);
    c.commit();
    out
}

fn check(inputs: Vec<(&str, Vec<u8>)>) -> verify::Report {
    let inputs: Vec<Input> = inputs.into_iter().map(|(n, b)| Input::new(n, b)).collect();
    verify::verify(&inputs, &Options { allow_test_key: true, ..Options::default() })
}

fn lines(r: &verify::Report, level: Level) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == level).map(|f| f.what.clone()).collect()
}

fn time_notes(r: &verify::Report) -> Vec<String> {
    lines(r, Level::Note).into_iter().filter(|n| n.contains("UTC")).filter(|n| !n.contains("no UTC time")).collect()
}

/// A steady GPS log: one record a second, UTC tracking uptime, with
/// corrections of a few hundred ms (too small for a clock event).
fn steady(base: u64) -> Vec<Step> {
    (1..=200u64)
        .map(|i| {
            let wobble = if i % 7 == 0 { 300 } else { 0 };
            Step::At(i * 1000, Some(base + i * 1000 + wobble), TimeSource::Gps)
        })
        .collect()
}

#[test]
fn a_steady_gps_log_has_nothing_to_say_about_time() {
    let r = check(vec![("a.bin", log(1, &steady(BUILT_MS + 86_400_000)))]);
    assert_eq!(r.exit_code(), 0, "{:?}", r.findings);
    assert!(time_notes(&r).is_empty(), "{:?}", time_notes(&r));
}

#[test]
fn a_log_with_no_gps_or_untrusted_time_still_verifies() {
    let mut steps: Vec<Step> = (1..50).map(|i| Step::At(i * 1000, None, TimeSource::None)).collect();
    steps.extend((50..80).map(|i| Step::At(i * 1000, None, TimeSource::Untrusted)));
    let r = check(vec![("a.bin", log(1, &steps))]);
    assert_eq!(r.exit_code(), 0, "{:?}", r.findings);
    assert!(time_notes(&r).is_empty(), "{:?}", time_notes(&r));
}

#[test]
fn utc_before_the_build_date_fails() {
    // The hub refuses GPS time before its build date (WP-12): a log that
    // carries such time as GPS time was not written by that firmware.
    let r = check(vec![("a.bin", log(1, &steady(BUILT_MS - 30 * 86_400_000)))]);
    let f = lines(&r, Level::Fail);
    assert!(f.iter().any(|f| f.contains("before this firmware was built")), "{f:?}");
    assert_eq!(r.exit_code(), 1);
}

#[test]
fn an_unexplained_jump_is_noted_and_an_explained_one_is_not() {
    let base = BUILT_MS + 86_400_000;
    let mut steps = steady(base);
    // An hour forward at 100 s, with no clock event.
    for s in steps.iter_mut() {
        if let Step::At(t, Some(u), _) = s
            && *t >= 100_000
        {
            *u += 3_600_000;
        }
    }
    let r = check(vec![("a.bin", log(1, &steps))]);
    assert_eq!(r.exit_code(), 0, "a note, not a failure: {:?}", r.findings);
    assert!(time_notes(&r).iter().any(|n| n.contains("jumped forward") && n.contains("3600")), "{:?}", time_notes(&r));
    // The same jump with the hub's clock event a second later: explained.
    steps.insert(100, Step::Clock(100_500, Some(base + 100_500 + 3_600_000)));
    let r = check(vec![("a.bin", log(1, &steps))]);
    assert!(time_notes(&r).is_empty(), "{:?}", time_notes(&r));
}

#[test]
fn utc_running_backwards_is_noted() {
    let base = BUILT_MS + 86_400_000;
    let mut steps = steady(base);
    for s in steps.iter_mut() {
        if let Step::At(t, Some(u), _) = s
            && *t >= 150_000
        {
            *u -= 60_000;
        }
    }
    let r = check(vec![("a.bin", log(1, &steps))]);
    assert!(time_notes(&r).iter().any(|n| n.contains("jumped back")), "{:?}", time_notes(&r));
}

#[test]
fn utc_running_at_the_wrong_rate_is_noted() {
    // 2 % fast against uptime, in steps too small to be called jumps.
    let base = BUILT_MS + 86_400_000;
    let steps: Vec<Step> = (1..=200u64).map(|i| Step::At(i * 1000, Some(base + i * 1020), TimeSource::Gps)).collect();
    let r = check(vec![("a.bin", log(1, &steps))]);
    assert!(time_notes(&r).iter().any(|n| n.contains("drifted")), "{:?}", time_notes(&r));
}

#[test]
fn time_going_backwards_from_one_boot_to_the_next_is_noted() {
    let mut u = common::unit::Unit::new();
    u.utc_base = Some(BUILT_MS + 10 * 86_400_000);
    let a = u.boot(1, 6, 0, "a");
    // The next boot's GPS says a day earlier.
    u.utc_base = Some(BUILT_MS + 9 * 86_400_000);
    let b = u.boot(2, 6, 0, "b");
    let r = check(vec![("a.bin", a.bytes), ("b.bin", b.bytes)]);
    assert!(lines(&r, Level::Fail).is_empty(), "{:?}", r.findings);
    assert!(time_notes(&r).iter().any(|n| n.contains("earlier than the end of the boot before")), "{:?}", time_notes(&r));
}
