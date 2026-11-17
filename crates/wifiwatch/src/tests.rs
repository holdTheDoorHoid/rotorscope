//! End-to-end: a mixed stream through [`Watch`].

use super::*;
use crate::mac::build::*;
use std::vec::Vec;

const AP: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];

#[test]
fn mixed_stream() {
    let mut w = Watch::new();
    let mut out: Vec<ThreatSighting> = Vec::new();
    let mut t = 0u64;
    // 10 s of normal life, then a deauth flood against AP with a twin.
    while t < 20_000 {
        let mut e = |s| out.push(s);
        w.frame(&wpa2(AP, b"Office"), -50, 6, t * 1000, &mut e);
        if t >= 10_000 {
            for _ in 0..5 {
                w.frame(&kill(mac::DEAUTH, util::BROADCAST, AP, AP, 7), -45, 6, t * 1000, &mut e);
            }
            w.frame(&open([0x02, 0xBA, 0xD0, 0, 0, 1], b"Office"), -40, 6, t * 1000 + 1, &mut e);
        }
        w.poll(t * 1000 + 50_000, &mut e);
        t += 100;
    }
    let kinds: Vec<_> = out.iter().map(|s| s.kind).collect();
    assert!(kinds.contains(&ThreatKind::Deauth), "{kinds:?}");
    assert!(kinds.contains(&ThreatKind::EvilTwin), "{kinds:?}");
    assert!(out.iter().all(|s| s.uptime_us >= 10_000_000), "nothing before the attack");
    // About once a second each, over ~10 s.
    let deauths = kinds.iter().filter(|&&k| k == ThreatKind::Deauth).count();
    let twins = kinds.iter().filter(|&&k| k == ThreatKind::EvilTwin).count();
    assert!((8..=11).contains(&deauths), "{deauths}");
    assert!((8..=11).contains(&twins), "{twins}");
    assert_eq!(w.floods.totals.deauth, 500);
    // Every sighting fits a link frame.
    let mut buf = [0u8; link::MAX_FRAME];
    for s in out {
        link::encode(&link::Frame::Threat(s), &mut buf).unwrap();
    }
}

#[test]
fn tools_table_exported() {
    assert_eq!(TOOLS[0], "Pwnagotchi");
}
