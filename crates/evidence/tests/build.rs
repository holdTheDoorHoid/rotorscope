//! What a boot event says about the build (WP-14a, HB-8 / HB-12): source
//! commit and tree state, build date; old boot events still read.

use evidence::format::{self, boot, CheckpointReason, EventCode};
use evidence::sign::SoftSigner;
use evidence::verify::{self, Input, Level, Options};
use evidence::writer::{Chain, Context};
use evidence::Signer;

fn log(ext: Option<boot::Ext>) -> Vec<u8> {
    let mut s = SoftSigner::public_test();
    let mut c = Chain::new([3; 8], [0xF1; 32]);
    let mut out = Vec::new();
    let mut b = [0u8; format::MAX_RECORD];
    let bd = boot::Boot { key_class: s.key_class() as u8, pubkey: s.public_key(), chip: (1, 3), ext };
    let mut data = [0u8; boot::LEN];
    let n = bd.encode(&mut data);
    let n = c.event(&Context::default(), EventCode::Boot, b"hub-p4 0.1.0 git x last reset: power-on; bench: none", &data[..n], &mut b).unwrap();
    out.extend_from_slice(&b[..n]);
    c.commit();
    let n = c.checkpoint(&Context { uptime_ms: 60_000, ..Context::default() }, CheckpointReason::Periodic, &mut s, &mut b).unwrap();
    out.extend_from_slice(&b[..n]);
    c.commit();
    out
}

fn check(bytes: Vec<u8>) -> verify::Report {
    verify::verify(&[Input::new("EV000001.EVL", bytes)], &Options { allow_test_key: true, ..Options::default() })
}

fn notes(r: &verify::Report) -> Vec<String> {
    r.findings.iter().filter(|f| f.level == Level::Note).map(|f| f.what.clone()).collect()
}

fn ext(git_state: u8) -> boot::Ext {
    boot::Ext { built_unix_s: 1_763_400_000, git: [0x66; 20], git_state, counter_source: boot::COUNTER_NONE, boot_id_method: boot::BOOT_ID_RNG }
}

#[test]
fn a_clean_build_names_its_commit_and_date() {
    let r = check(log(Some(ext(boot::GIT_CLEAN))));
    assert_eq!(r.exit_code(), 0, "{:?}", r.findings);
    assert_eq!(r.chains[0].build, Some(ext(boot::GIT_CLEAN)));
    assert!(!notes(&r).iter().any(|n| n.contains("source commit")), "{:?}", notes(&r));
}

#[test]
fn a_dirty_or_unknown_build_is_said() {
    let r = check(log(Some(ext(boot::GIT_DIRTY))));
    assert_eq!(r.exit_code(), 0);
    assert!(notes(&r).iter().any(|n| n.contains("WITH CHANGES")), "{:?}", notes(&r));
    let mut e = ext(boot::GIT_UNKNOWN);
    e.git = [0; 20];
    let r = check(log(Some(e)));
    assert!(notes(&r).iter().any(|n| n.contains("did not record its source commit")), "{:?}", notes(&r));
}

#[test]
fn a_boot_event_from_before_wp14a_still_verifies() {
    let r = check(log(None));
    assert_eq!(r.exit_code(), 0, "{:?}", r.findings);
    assert_eq!(r.chains[0].build, None);
    assert!(notes(&r).iter().any(|n| n.contains("predates WP-14a")), "{:?}", notes(&r));
}
