//! Fuzz-style tests (crates/fuzzkit) for both ends of the wired bus: the
//! master reading an answer (or a join) from the return wire, the node
//! reading the hub's command stream, and the attributor that names the
//! sender of every frame.

use std::vec::Vec;

use link::Command;

use crate::master::Event;
use crate::sim::{Kind, make_frame, mac_of};
use crate::tests::{Q, joined_pair};
use crate::{Attributor, MAX_ANSWER, MAX_UNIT, chunk_tag, is_via};

const ITERS: usize = 3_000;

/// A real answer to a real poll: frames, then the BusDone with its CRC.
fn answer_seed() -> (Vec<u8>, Vec<u8>) {
    let (mut m, mut n) = joined_pair();
    let mut q = Q::default();
    for (i, k) in [Kind::RidPack, Kind::Threat, Kind::Stats].into_iter().enumerate() {
        q.frame(&make_frame(k, i as u64 + 1, &mut crate::sim::Rng::new(i as u64)));
    }
    let poll = m.tick(100_000, 0, &mut |_| {}).unwrap().to_vec();
    let ans = n.on_rx(100_100, &poll, &mut q, &mut |_| {}).unwrap().to_vec();
    (poll, ans)
}

#[test]
fn fuzz_master_answer() {
    let (_, ans) = answer_seed();
    let cfg = fuzzkit::Config::new().max_len(MAX_ANSWER + 64);
    fuzzkit::run_with("wirebus::Master::on_rx", &cfg, &[&ans], ITERS, |bytes| {
        let (mut m, _) = joined_pair();
        let _ = m.tick(100_000, 0, &mut |_| {}).unwrap();
        let joined = mac_of(0);
        let mut attr = Attributor::new();
        let mut out: Vec<Vec<u8>> = Vec::new();
        // Split at a point the input picks, as a UART delivers it.
        let cut = bytes.first().map_or(0, |&b| usize::from(b)).min(bytes.len());
        for (k, part) in [&bytes[..cut], &bytes[cut..]].into_iter().enumerate() {
            m.on_rx(100_500 + k as u64 * 50, part, &mut |e| match e {
                Event::Frame { mac, addr, bytes: f } => {
                    assert_eq!(mac, joined, "a frame credited to a node that was not polled");
                    assert_eq!(addr, 1);
                    assert_eq!(f.last(), Some(&0));
                    assert!(f.len() <= MAX_UNIT);
                    attr.frame(mac, f, &mut |b| out.push(b.to_vec()));
                }
                Event::AnswerEnd { mac, .. } => {
                    assert_eq!(mac, joined);
                    attr.end_answer();
                }
                Event::Joined { addr, .. } | Event::Absent { addr, .. } | Event::Back { addr, .. } => {
                    assert!((1..=crate::MAX_ADDR).contains(&addr));
                }
            });
        }
        // Every frame passed on is a node's, under the polled node's own Via.
        let mut it = out.iter();
        while let Some(v) = it.next() {
            assert!(is_via(v), "a frame passed on without its Via");
            match link::origin::decode_chunk(v) {
                Some(link::Frame::Via { node, .. }) => assert_eq!(node, joined),
                other => panic!("not a Via: {other:?}"),
            }
            let f = it.next().expect("a Via with no frame");
            assert!(matches!(link::origin::origin_of_encoded(f), None | Some(link::origin::Origin::Node)));
        }
        let _ = m.tick(200_000, 0, &mut |_| {});
    });
}

#[test]
fn fuzz_master_join_and_idle() {
    // A real join, from a node answering the boot join slots.
    let join = {
        let mut m: crate::Master<4> = crate::Master::new(crate::MasterConfig::new(), 7);
        let mut n = crate::Node::new(crate::NodeConfig::new(), mac_of(3), 5, 0);
        let slot = m.tick(0, 0, &mut |_| {}).unwrap().to_vec();
        assert!(n.on_rx(0, &slot, &mut Q::default(), &mut |_| {}).is_none());
        let t = n.next_deadline().unwrap();
        n.tick(t).unwrap().to_vec()
    };
    let (_, ans) = answer_seed();
    let seeds: [&[u8]; 2] = [&join, &ans];
    fuzzkit::run("wirebus::Master::on_rx(join)", &seeds, ITERS, |bytes| {
        let mut m: crate::Master<4> = crate::Master::new(crate::MasterConfig::new(), 7);
        // At boot the master opens join slots: the bytes land in one.
        let _ = m.tick(0, 0, &mut |_| {});
        m.on_rx(300, bytes, &mut |e| {
            if let Event::Joined { addr, .. } = e {
                assert!((1..=crate::MAX_ADDR).contains(&addr));
            }
        });
        let _ = m.tick(50_000, 0, &mut |_| {});
        // Idle: unasked bytes.
        m.on_rx(60_000, bytes, &mut |_| {});
        let _ = m.looks_legacy(60_000);
        assert!(m.entries().len() <= 4);
    });
}

#[test]
fn fuzz_node_commands() {
    let (poll, _) = answer_seed();
    let mut b = [0u8; 400];
    let mut stream = poll.clone();
    for c in [
        Command::To { node: mac_of(0) },
        Command::Assign(link::Assignment { role: link::NodeRole::Ble, plan: heapless::Vec::new(), id: 9 }),
        Command::TimeSync { utc_ms: 1_700_000_000_000, pps: true },
        Command::Address { mac: mac_of(0), addr: 2, epoch: 0x1234, check: 0 },
        Command::JoinSlot { epoch: 0x1234, slots: 4, slot_us: 300 },
    ] {
        stream.extend_from_slice(link::encode_command(&c, &mut b).unwrap());
    }
    fuzzkit::run_with("wirebus::Node::on_rx", &fuzzkit::Config::new().max_len(800), &[&poll, &stream], ITERS, |bytes| {
        let (_, mut n) = joined_pair();
        let mut q = Q::default();
        q.frame(&make_frame(Kind::RidFull, 1, &mut crate::sim::Rng::new(1)));
        q.frame(&make_frame(Kind::Stats, 2, &mut crate::sim::Rng::new(2)));
        let mut assigns = 0;
        if let Some(a) = n.on_rx(200_000, bytes, &mut q, &mut |_| assigns += 1) {
            assert!(a.len() <= MAX_ANSWER);
            assert_eq!(a.last(), Some(&0));
        }
        n.sent();
        let _ = n.take_time_sync();
        if let Some(a) = n.tick(n.next_deadline().unwrap_or(200_000).max(200_000)) {
            assert!(a.len() <= MAX_ANSWER);
        }
        if let Some(addr) = n.addr() {
            assert!((1..=crate::MAX_ADDR).contains(&addr));
        }
    });
}

#[test]
fn fuzz_attributor_and_chunk_tag() {
    let mut buf = [0u8; 512];
    let mut seeds: Vec<Vec<u8>> = Vec::new();
    for f in [
        link::Frame::Via { node: mac_of(5), rssi: -40 },
        make_frame(Kind::RidPack, 1, &mut crate::sim::Rng::new(3)),
        make_frame(Kind::Threat, 2, &mut crate::sim::Rng::new(4)),
    ] {
        seeds.push(link::encode(&f, &mut buf).unwrap().to_vec());
    }
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    fuzzkit::run("wirebus::Attributor::frame", &refs, ITERS * 2, |bytes| {
        let _ = chunk_tag(bytes);
        let gw = mac_of(9);
        for mac in [mac_of(1), gw] {
            let mut a = Attributor::new();
            assert!(a.register_gateway(gw));
            let mut out: Vec<Vec<u8>> = Vec::new();
            // The same bytes twice: a Via then its frame, from one sender.
            for part in bytes.split_inclusive(|&b| b == 0).chain(bytes.split_inclusive(|&b| b == 0)) {
                a.frame(mac, part, &mut |b| out.push(b.to_vec()));
            }
            a.end_answer();
            // A node that is not a gateway never names another sender.
            if mac != gw {
                for o in &out {
                    if let Some(link::Frame::Via { node, .. }) = link::origin::decode_chunk(o) {
                        assert_eq!(node, mac, "a node attributed a frame to another address");
                    }
                }
            }
        }
    });
}

/// The answer seed reaches the deep path: the master hands on its frames.
#[test]
fn answer_seed_is_accepted() {
    let (_, ans) = answer_seed();
    let (mut m, _) = joined_pair();
    let _ = m.tick(100_000, 0, &mut |_| {}).unwrap();
    let mut frames = 0;
    m.on_rx(100_500, &ans, &mut |e| frames += matches!(e, Event::Frame { .. }) as u32);
    assert_eq!(frames, 3);
}
