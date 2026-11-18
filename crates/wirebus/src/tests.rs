//! Protocol tests: unit tests of each end, then the simulated wire.
//! `cargo test -p wirebus -- --nocapture` prints the timing tables used in
//! docs/WIREBUS.md.

use std::vec::Vec;

use link::{Assignment, Command, Frame, NodeRole};

use crate::master::{Event, Master, MasterConfig};
use crate::node::{Node, NodeConfig, Outbox};
use crate::sim::{Kind, MS, SEC, Sim, Traffic, US, make_frame, mac_of};
use crate::*;

// ------------------------------------------------------------ unit tests --

#[test]
fn tags_match_link() {
    let mut buf = [0u8; 64];
    let join = Frame::BusJoin { mac: [1; 6], want: 0, check: 0 };
    let done = Frame::BusDone { addr: 1, seq: 0, more: false, crc: 0 };
    let via = Frame::Via { node: [1; 6], rssi: -1 };
    assert_eq!(chunk_tag(link::encode(&join, &mut buf).unwrap()), Some(TAG_BUS_JOIN));
    assert_eq!(chunk_tag(link::encode(&done, &mut buf).unwrap()), Some(TAG_BUS_DONE));
    assert!(is_via(link::encode(&via, &mut buf).unwrap()));
    // Tag 0 (Hello) starts with COBS code 1.
    let hello = Frame::Hello { role: NodeRole::Ble, protocol: 10, firmware: heapless::String::new() };
    assert_eq!(chunk_tag(link::encode(&hello, &mut buf).unwrap()), Some(0));
    assert_eq!(chunk_tag(&[]), None);
    assert_eq!(chunk_tag(&[0]), None);
    // The largest BusDone / BusJoin fit MAX_DONE.
    let big = Frame::BusDone { addr: 255, seq: 255, more: true, crc: u16::MAX };
    assert!(link::encode(&big, &mut buf).unwrap().len() <= MAX_DONE);
    let big = Frame::BusJoin { mac: [0xFF; 6], want: 255, check: u16::MAX };
    assert!(link::encode(&big, &mut buf).unwrap().len() <= MAX_DONE);
}

#[test]
fn attribution_matches_a_gateway_stream() {
    // A wired node's frames get a Via each; a gateway on the bus keeps its own.
    let mut b = [0u8; 64];
    let me = [1, 1, 1, 1, 1, 1];
    let other = [2, 2, 2, 2, 2, 2];
    let f1 = link::encode(&stats_frame(1), &mut b).unwrap().to_vec();
    let via = link::encode(&Frame::Via { node: other, rssi: -40 }, &mut b).unwrap().to_vec();
    let mut a = Attributor::new();
    assert!(a.register_gateway(me));
    let mut stream = Vec::new();
    for chunk in [&f1, &via, &f1, &f1] {
        a.frame(me, chunk, &mut |x| stream.extend_from_slice(x));
    }
    let mut got = Vec::new();
    link::Decoder::new().feed(&stream, |r| got.push(r.unwrap()));
    let v = |node| Frame::Via { node, rssi: 0 };
    assert_eq!(got, [v(me), stats_frame(1), Frame::Via { node: other, rssi: -40 }, stats_frame(1), v(me), stats_frame(1)]);
    assert_eq!((a.added, a.relayed), (2, 1));
    // A dangling Via at the end of an answer does not carry over.
    a.frame(me, &via, &mut |_| {});
    a.end_answer();
    let mut n = 0;
    a.frame(me, &f1, &mut |_| n += 1);
    assert_eq!(n, 2, "our own Via, then the frame");
}

#[test]
fn wire_time() {
    assert_eq!(wire_us(1, 921_600), 11);
    assert_eq!(wire_us(92_160, 921_600), 1_000_000);
    assert_eq!(MasterConfig::new().cycle_bytes(), 1843);
}

/// A queue of encoded frames for unit tests.
#[derive(Default)]
struct Q(std::collections::VecDeque<Vec<u8>>);

impl Q {
    fn frame(&mut self, f: &Frame) {
        let mut b = [0u8; link::MAX_FRAME];
        self.0.push_back(link::encode(f, &mut b).unwrap().to_vec());
    }
}

impl Outbox for Q {
    fn peek(&mut self) -> Option<&[u8]> {
        self.0.front().map(|v| &v[..])
    }
    fn pop(&mut self) {
        self.0.pop_front();
    }
}

fn cmd(c: &Command) -> Vec<u8> {
    let mut b = [0u8; 400];
    link::encode_command(c, &mut b).unwrap().to_vec()
}

fn stats_frame(id: u64) -> Frame {
    Frame::Stats { uptime_us: id, frames_seen: 1, rid_found: 0, dropped: 0, by_transport: heapless::Vec::new() }
}

/// Join slots only at boot, so hand-driven tests see polls.
fn rare_joins() -> MasterConfig {
    MasterConfig { join_fast_us: 600_000_000, join_every_us: 600_000_000, ..MasterConfig::new() }
}

/// Join one node to a master by hand; returns (master, node).
fn joined_pair() -> (Master<4>, Node) {
    let mut m: Master<4> = Master::new(rare_joins(), 0x1234);
    let mut n = Node::new(NodeConfig::new(), mac_of(0), 7, 0);
    let mut q = Q::default();
    let mut ev = |_: Event<'_>| {};
    let slot = m.tick(0, 0, &mut ev).unwrap().to_vec();
    assert!(n.on_rx(0, &slot, &mut q, &mut |_| {}).is_none());
    let t = n.next_deadline().unwrap();
    let join = n.tick(t).unwrap().to_vec();
    m.on_rx(t + 200, &join, &mut ev);
    let grant = m.tick(t + 20_000, 0, &mut ev).unwrap().to_vec();
    n.on_rx(t + 20_100, &grant, &mut q, &mut |_| {});
    assert_eq!(n.addr(), Some(1));
    (m, n)
}

#[test]
fn answer_is_acknowledged_and_never_split() {
    let (mut m, mut n) = joined_pair();
    let mut q = Q::default();
    // Units bigger than the budget still go, one per answer.
    for id in 1..=3 {
        q.frame(&make_frame(Kind::RidFull, id, &mut crate::sim::Rng::new(id)));
    }
    let mut got = Vec::new();
    let mut now = 100_000;
    for _ in 0..3 {
        let poll = m.tick(now, 0, &mut |_| {}).unwrap().to_vec();
        let ans = n.on_rx(now + 100, &poll, &mut q, &mut |_| {}).unwrap().to_vec();
        n.sent();
        // Whole frames only: every zero ends a frame that decodes.
        m.on_rx(now + 1000, &ans, &mut |e| {
            if let Event::Frame { bytes, .. } = e {
                let mut d = link::Decoder::new();
                d.feed(bytes, |r| got.push(crate::sim::frame_id(&r.unwrap()).unwrap()));
            }
        });
        now += 25_000;
    }
    // One node gets the 1024-byte budget: all three full packs (~260 B
    // each) go in the first answer, the next two answers are empty.
    assert_eq!(got, [1, 2, 3]);
    assert_eq!(m.entries()[0].stats.answers, 3);
    assert_eq!(m.entries()[0].stats.empty, 2);
}

#[test]
fn small_budget_still_sends_one_whole_unit() {
    let (_, mut n) = joined_pair();
    let mut q = Q::default();
    for id in 1..=2 {
        q.frame(&make_frame(Kind::RidFull, id, &mut crate::sim::Rng::new(id)));
    }
    let poll = cmd(&Command::Poll { addr: 1, epoch: 0x1234, ack: 0, budget: 10 });
    let ans = n.on_rx(10, &poll, &mut q, &mut |_| {}).unwrap().to_vec();
    assert_eq!(ans.iter().filter(|&&b| b == 0).count(), 2, "one frame + BusDone");
    assert_eq!(q.0.len(), 1);
}

#[test]
fn unacknowledged_answer_is_resent_and_duplicate_dropped() {
    let (mut m, mut n) = joined_pair();
    let mut q = Q::default();
    q.frame(&stats_frame(1));
    let mut frames = 0;
    let poll = m.tick(100_000, 0, &mut |_| {}).unwrap().to_vec();
    let ans = n.on_rx(100_100, &poll, &mut q, &mut |_| {}).unwrap().to_vec();
    n.sent();
    m.on_rx(100_500, &ans, &mut |e| frames += matches!(e, Event::Frame { .. }) as u32);
    assert_eq!(frames, 1);
    // The next poll (with the ack) is lost on the way; a poll that still
    // carries the old ack arrives instead: the node resends, the hub drops it.
    let stale_ack = cmd(&Command::Poll { addr: 1, epoch: 0x1234, ack: 0, budget: 500 });
    q.frame(&stats_frame(2));
    let again = n.on_rx(200_000, &stale_ack, &mut q, &mut |_| {}).unwrap().to_vec();
    n.sent();
    // Same frames byte for byte; only the BusDone differs (`more` is now set).
    let body = |a: &[u8]| a[..a[..a.len() - 1].iter().rposition(|&b| b == 0).unwrap() + 1].to_vec();
    assert_eq!(body(&again), body(&ans), "same frames, byte for byte");
    let _ = m.tick(200_000, 0, &mut |_| {}).unwrap();
    m.on_rx(200_500, &again, &mut |e| frames += matches!(e, Event::Frame { .. }) as u32);
    assert_eq!(frames, 1);
    assert_eq!(m.entries()[0].stats.dups, 1);
    // The real next poll acknowledges; frame 2 goes.
    let poll = m.tick(300_000, 0, &mut |_| {}).unwrap().to_vec();
    let ans = n.on_rx(300_100, &poll, &mut q, &mut |_| {}).unwrap().to_vec();
    n.sent();
    m.on_rx(300_500, &ans, &mut |e| frames += matches!(e, Event::Frame { .. }) as u32);
    assert_eq!(frames, 2);
    assert_eq!(n.counters().resends, 1);
}

#[test]
fn garbage_then_delimiter_resyncs() {
    let (mut m, mut n) = joined_pair();
    let mut q = Q::default();
    q.frame(&stats_frame(9));
    let poll = m.tick(100_000, 0, &mut |_| {}).unwrap().to_vec();
    let ans = n.on_rx(100_100, &poll, &mut q, &mut |_| {}).unwrap().to_vec();
    n.sent();
    // Junk without a zero runs into the first frame: the answer fails its CRC.
    let mut bad = vec![0x55, 0xAA, 0x13];
    bad.extend_from_slice(&ans);
    let mut frames = 0;
    m.on_rx(100_600, &bad, &mut |e| frames += matches!(e, Event::Frame { .. }) as u32);
    assert_eq!((frames, m.entries()[0].stats.corrupt), (0, 1));
    // Junk *with* a zero is its own chunk: still the whole answer is refused
    // (the CRC covers everything before BusDone), and the resend gets through.
    let poll = m.tick(102_000, 0, &mut |_| {}).unwrap().to_vec();
    let ans2 = n.on_rx(102_100, &poll, &mut q, &mut |_| {}).unwrap().to_vec();
    n.sent();
    assert_eq!(ans2, ans);
    m.on_rx(102_600, &ans2, &mut |e| frames += matches!(e, Event::Frame { .. }) as u32);
    assert_eq!(frames, 1);
    // A lone zero before an answer (a glitch as the driver starts) is ignored.
    q.frame(&stats_frame(10));
    let poll = m.tick(130_000, 0, &mut |_| {}).unwrap().to_vec();
    let ans3 = n.on_rx(130_100, &poll, &mut q, &mut |_| {}).unwrap().to_vec();
    n.sent();
    let mut z = vec![0, 0];
    z.extend_from_slice(&ans3);
    m.on_rx(130_600, &z, &mut |e| frames += matches!(e, Event::Frame { .. }) as u32);
    assert_eq!(frames, 2);
}

#[test]
fn assign_only_for_the_named_node() {
    let (_, mut n) = joined_pair();
    let mut q = Q::default();
    let a = Assignment { role: NodeRole::Ble, plan: heapless::Vec::new(), id: 7 };
    let mut got = Vec::new();
    let mut bytes = cmd(&Command::To { node: [9; 6] });
    bytes.extend(cmd(&Command::Assign(a.clone())));
    // Bare Assign (no To) is not taken on the bus either.
    bytes.extend(cmd(&Command::Assign(a.clone())));
    bytes.extend(cmd(&Command::To { node: mac_of(0) }));
    bytes.extend(cmd(&Command::Assign(a.clone())));
    n.on_rx(5, &bytes, &mut q, &mut |x| got.push(x));
    assert_eq!(got, [a]);
}

/// Protocol 11: `TimeSync` is a broadcast, taken with or without a `To`,
/// stamped with the arrival time; only the latest is kept.
#[test]
fn time_sync_is_a_broadcast() {
    let (_, mut n) = joined_pair();
    let mut q = Q::default();
    assert_eq!(n.take_time_sync(), None);
    let mut bytes = cmd(&Command::TimeSync { utc_ms: 1_000, pps: false });
    bytes.extend(cmd(&Command::TimeSync { utc_ms: 2_000, pps: true }));
    n.on_rx(77, &bytes, &mut q, &mut |_| panic!("not an assign"));
    assert_eq!(n.take_time_sync(), Some(TimeSyncRx { at_us: 77, utc_ms: 2_000, pps: true }));
    assert_eq!(n.take_time_sync(), None);
    let mut bytes = cmd(&Command::To { node: [9; 6] });
    bytes.extend(cmd(&Command::TimeSync { utc_ms: 3_000, pps: false }));
    n.on_rx(90, &bytes, &mut q, &mut |_| {});
    assert_eq!(n.take_time_sync().map(|t| t.utc_ms), Some(3_000));
}

#[test]
fn stale_poll_is_not_answered() {
    let (mut m, mut n) = joined_pair();
    let mut q = Q::default();
    q.frame(&stats_frame(1));
    let mut poll = m.tick(100_000, 0, &mut |_| {}).unwrap().to_vec();
    // The hub gave up on us and polled someone else before we read the line.
    poll.extend(cmd(&Command::Poll { addr: 2, epoch: 0x1234, ack: 0, budget: 300 }));
    assert!(n.on_rx(110_000, &poll, &mut q, &mut |_| {}).is_none());
    assert_eq!(n.counters().stale, 1);
    // Fed in two pieces (the firmware's check for newer bytes) it is the same.
    let p1 = cmd(&Command::Poll { addr: 1, epoch: 0x1234, ack: 0, budget: 300 });
    assert!(n.on_rx(120_000, &p1, &mut q, &mut |_| {}).is_some());
    let p2 = cmd(&Command::Poll { addr: 2, epoch: 0x1234, ack: 0, budget: 300 });
    assert!(n.on_rx(120_001, &p2, &mut q, &mut |_| {}).is_none());
    assert_eq!(n.counters().stale, 2);
    // The frame is still held and goes with the next poll.
    assert!(n.holding());
}

#[test]
fn other_epoch_means_join_again() {
    let (_, mut n) = joined_pair();
    let mut q = Q::default();
    let poll = cmd(&Command::Poll { addr: 1, epoch: 0x9999, ack: 0, budget: 300 });
    assert!(n.on_rx(10, &poll, &mut q, &mut |_| {}).is_none());
    assert_eq!(n.addr(), None);
    // It asks for its old address back.
    let slot = cmd(&Command::JoinSlot { epoch: 0x9999, slots: 1, slot_us: 500 });
    n.on_rx(20, &slot, &mut q, &mut |_| {});
    let j = n.tick(n.next_deadline().unwrap()).unwrap().to_vec();
    let f: Frame = postcard::from_bytes_cobs(&mut j.clone()).unwrap();
    assert!(matches!(f, Frame::BusJoin { want: 1, .. }));
}

#[test]
fn garbled_grant_is_ignored_and_conflict_drops_address() {
    let (_, mut n) = joined_pair();
    let mut q = Q::default();
    let bad = cmd(&Command::Address { mac: mac_of(0), addr: 5, epoch: 0x1234, check: 1 });
    n.on_rx(1, &bad, &mut q, &mut |_| {});
    assert_eq!(n.addr(), Some(1));
    let other = mac_of(7);
    let theirs = cmd(&Command::Address { mac: other, addr: 1, epoch: 0x1234, check: crc::address_check(&other, 1, 0x1234) });
    n.on_rx(2, &theirs, &mut q, &mut |_| {});
    assert_eq!(n.addr(), None);
}

#[test]
fn silence_drops_address() {
    let (_, mut n) = joined_pair();
    let t = n.next_deadline().unwrap();
    n.tick(t + 1);
    assert_eq!(n.addr(), None);
}

#[test]
fn legacy_stream_is_recognised() {
    let mut m: Master<4> = Master::new(MasterConfig::new(), 1);
    // An old gateway streams Via + Hello on its own, never polled.
    let mut b = [0u8; 300];
    let mut t = 0;
    let _ = m.tick(t, 0, &mut |_| {});
    t += 50_000; // the join window has closed
    let _ = m.tick(t, 0, &mut |_| {});
    assert!(!m.looks_legacy(t));
    for _ in 0..4 {
        let w = link::encode(&Frame::Via { node: [2; 6], rssi: -50 }, &mut b).unwrap().to_vec();
        m.on_rx(t, &w, &mut |_| {});
        let w = link::encode(&stats_frame(1), &mut b).unwrap().to_vec();
        m.on_rx(t + 10, &w, &mut |_| {});
        t += 100_000;
    }
    // While a poll was pending some of these would have counted as answers;
    // here the table is empty, so all are stray.
    assert!(m.stats.stray_frames >= 3);
    assert!(m.looks_legacy(t));
}

#[test]
fn table_full_evicts_absent_node() {
    let mut m: Master<2> = Master::new(MasterConfig::new(), 3);
    let mut now = 0u64;
    // Tick (letting polls time out, nobody answers) until a join slot goes
    // out, answer it with `mac`, and return the address granted.
    let join = |m: &mut Master<2>, mac: [u8; 6], now: &mut u64| -> Option<u8> {
        for _ in 0..100_000 {
            *now += 500;
            let Some(b) = m.tick(*now, 0, &mut |_| {}) else { continue };
            let mut b = b.to_vec();
            if !matches!(postcard::from_bytes_cobs::<Command>(&mut b), Ok(Command::JoinSlot { .. })) {
                continue;
            }
            let mut f = [0u8; 32];
            let j = link::encode(&Frame::BusJoin { mac, want: 0, check: crc::join_check(&mac, 0) }, &mut f).unwrap().to_vec();
            m.on_rx(*now + 1000, &j, &mut |_| {});
            *now += 20_000;
            let mut granted = None;
            let _ = m.tick(*now, 0, &mut |e| {
                if let Event::Joined { addr, .. } = e {
                    granted = Some(addr)
                }
            });
            return granted;
        }
        None
    };
    assert_eq!(join(&mut m, mac_of(0), &mut now), Some(1));
    assert_eq!(join(&mut m, mac_of(1), &mut now), Some(2));
    // Both are present (just joined): a third is refused...
    assert_eq!(join(&mut m, mac_of(2), &mut now), None);
    assert_eq!(m.stats.join_full, 1);
    // ...until they have missed enough polls to be absent.
    while !m.entries().iter().all(|e| e.absent) {
        now += 1000;
        let _ = m.tick(now, 0, &mut |_| {});
    }
    assert!(join(&mut m, mac_of(2), &mut now).is_some());
    assert!(m.entry(&mac_of(2)).is_some());
    assert_eq!(m.entries().len(), 2);
    // A node that rejoins gets its address back, whatever it asks for.
    let a = m.entry(&mac_of(2)).unwrap().addr;
    assert_eq!(join(&mut m, mac_of(2), &mut now), Some(a));
}

// ------------------------------------------------------ simulated wire --

fn settle(s: &mut Sim) {
    let mut t = 0;
    while !s.all_joined() {
        s.run(10 * MS);
        t += 10;
        assert!(t < 10_000, "not all joined after 10 s: {:?}", s.nodes.iter().map(|n| n.node.addr()).collect::<Vec<_>>());
    }
}

fn distinct_addrs(s: &Sim) -> bool {
    let mut a: Vec<u8> = s.nodes.iter().filter_map(|n| n.node.addr()).collect();
    a.sort_unstable();
    let len = a.len();
    a.dedup();
    a.len() == len
}

/// No frame lost or repeated: generated = delivered + dropped by full
/// queues + still waiting (+ lost in node reboots).
fn assert_accounted(s: &Sim) {
    let (generated, delivered, dropped, left) = s.accounting();
    assert_eq!(generated, delivered + dropped + left + s.lost_at_reboot, "gen {generated} del {delivered} drop {dropped} left {left} reboot {}", s.lost_at_reboot);
    assert_eq!(s.undecodable, 0);
    s.check_order().unwrap();
}

#[test]
fn eight_nodes_join_through_collisions() {
    let mut worst_ms = 0;
    let mut collided = 0;
    for seed in 1..=6 {
        let mut s = Sim::new(8, Traffic::quiet(), seed);
        let t0 = s.now;
        settle(&mut s);
        worst_ms = worst_ms.max((s.now - t0) / MS);
        assert!(distinct_addrs(&s));
        collided += s.master.stats.join_corrupt;
        // Addresses tied to MACs: the hub's table matches the nodes.
        for n in &s.nodes {
            assert_eq!(s.master.entry(&n.mac).map(|e| e.addr), n.node.addr());
        }
    }
    println!("8 nodes powered at once: all joined within {worst_ms} ms (6 seeds), {collided} garbled join chunks along the way");
    assert!(collided > 0, "eight nodes at once should collide at least once");
    assert!(worst_ms < 3_000);
}

#[test]
fn delivery_exact_and_ordered_under_noise() {
    let mut s = Sim::new(4, Traffic::busy(), 11);
    s.noise_ret = 2e-4;
    s.noise_hub_tx = 2e-4;
    settle(&mut s);
    s.run(10 * SEC);
    // Drain: stop traffic, let the queues empty.
    for n in s.nodes.iter_mut() {
        n.traffic = Traffic::default();
    }
    s.run(SEC);
    let corrupt: u32 = s.master.entries().iter().map(|e| e.stats.corrupt).sum();
    let timeouts: u32 = s.master.entries().iter().map(|e| e.stats.timeouts).sum();
    println!("noise 2e-4 both wires, 4 busy nodes, 10 s: {} frames, {corrupt} answers refused by CRC, {timeouts} silent polls, all resent", s.delivered.len());
    assert!(corrupt > 0, "the noise should have hit some answers");
    assert_accounted(&s);
    let (_, _, dropped, left) = s.accounting();
    assert_eq!((dropped, left), (0, 0));
}

#[test]
fn heavy_noise_mid_frame_still_exact() {
    let mut s = Sim::new(2, Traffic::busy(), 5);
    s.noise_ret = 2e-3;
    settle(&mut s);
    s.run(5 * SEC);
    for n in s.nodes.iter_mut() {
        n.traffic = Traffic::default();
    }
    s.run(2 * SEC);
    assert_accounted(&s);
    let corrupt: u32 = s.master.entries().iter().map(|e| e.stats.corrupt).sum();
    assert!(corrupt > 20, "{corrupt}");
}

#[test]
fn node_reboot_and_image_switch_keep_address() {
    let mut s = Sim::new(4, Traffic::busy(), 21);
    settle(&mut s);
    s.run(2 * SEC);
    let before: Vec<_> = s.nodes.iter().map(|n| n.node.addr()).collect();
    // Node 2 switches image (a reboot of ~3 s with the same MAC).
    s.nodes[2].plugged = false;
    s.reboot_node(2);
    s.run(3 * SEC);
    s.nodes[2].plugged = true;
    let mark = s.joined.len();
    s.run(3 * SEC);
    let after: Vec<_> = s.nodes.iter().map(|n| n.node.addr()).collect();
    assert_eq!(before, after);
    assert!(s.joined[mark..].iter().any(|j| j.0 == mac_of(2) && j.2), "rejoined as the same node");
    assert_eq!(s.master.entry(&mac_of(2)).unwrap().stats.joins, 2);
    // A plain reboot without unplugging, too.
    s.reboot_node(1);
    s.run(3 * SEC);
    assert_eq!(s.nodes[1].node.addr(), before[1]);
    for n in s.nodes.iter_mut() {
        n.traffic = Traffic::default();
    }
    s.run(SEC);
    assert_accounted(&s);
}

#[test]
fn hub_reboot_gives_the_same_addresses_back() {
    let mut s = Sim::new(6, Traffic::busy(), 31);
    settle(&mut s);
    s.run(2 * SEC);
    let before: Vec<_> = s.nodes.iter().map(|n| n.node.addr()).collect();
    let n0 = s.delivered.len();
    s.reboot_hub();
    s.run(4 * SEC);
    assert!(s.all_joined());
    let after: Vec<_> = s.nodes.iter().map(|n| n.node.addr()).collect();
    assert_eq!(before, after, "each node asked for its old address and got it");
    assert!(s.delivered.len() > n0 + 100);
    for n in s.nodes.iter_mut() {
        n.traffic = Traffic::default();
    }
    s.run(SEC);
    // An answer the old hub took but had not acknowledged is sent again to
    // the new one: at most one answer's frames repeated per node, nothing
    // lost, and everything new still in order.
    let mut repeats = 0u64;
    for n in &s.nodes {
        let mut seen = std::collections::BTreeSet::new();
        let mut top = 0;
        let mut node_repeats = 0;
        for &id in &n.sent_ids {
            if !seen.insert(id) {
                node_repeats += 1;
                continue;
            }
            assert!(id > top, "out of order");
            top = id;
        }
        assert!(node_repeats <= 32, "{node_repeats}");
        repeats += node_repeats;
    }
    println!("hub reboot: {repeats} frames repeated in total over {} nodes", s.nodes.len());
    let (generated, delivered, dropped, left) = s.accounting();
    assert_eq!(generated + repeats, delivered + dropped + left);
}

#[test]
fn unplugged_node_goes_absent_and_others_carry_on() {
    let mut s = Sim::new(4, Traffic::busy(), 41);
    settle(&mut s);
    s.run(2 * SEC);
    s.nodes[1].plugged = false;
    s.run(2 * SEC);
    assert!(s.absent.contains(&mac_of(1)));
    assert!(s.master.entry(&mac_of(1)).unwrap().absent);
    let mark = s.delivered.len();
    s.run(5 * SEC);
    let (_, _, p99, max) = s.latency(mark);
    println!("one of 4 nodes unplugged: others p99 {} ms, max {} ms", p99 / MS, max / MS);
    assert!(p99 < 40 * MS);
    let addr = s.nodes[1].node.addr();
    s.nodes[1].plugged = true;
    s.run(4 * SEC);
    assert!(s.all_joined());
    assert!(!s.master.entry(&mac_of(1)).unwrap().absent);
    assert!(addr.is_none() || s.nodes[1].node.addr() == addr);
    assert_eq!(s.master.entry(&mac_of(1)).unwrap().addr, s.nodes[1].node.addr().unwrap());
}

#[test]
fn flooding_node_cannot_starve_the_others() {
    let mut s = Sim::new(4, Traffic::busy(), 51);
    s.nodes[0].traffic = Traffic::flood(2000.0);
    settle(&mut s);
    let mark = s.delivered.len();
    s.run(5 * SEC);
    let others: Vec<u64> = s.delivered[mark..].iter().filter(|d| d.node != 0).map(|d| d.latency_ns).collect();
    let mut o = others.clone();
    o.sort_unstable();
    let p99 = o[(o.len() - 1) * 99 / 100];
    let flood = s.delivered[mark..].iter().filter(|d| d.node == 0).count();
    println!(
        "node 0 flooding 2000 full packs/s: it got {} frames/s through, the others' p99 {} ms; longest answer {} B (budget {})",
        flood / 5,
        p99 / MS,
        s.nodes[0].max_answer,
        s.master.budget()
    );
    assert!(p99 < 50 * MS, "{} ms", p99 / MS);
    for n in &s.nodes[1..] {
        assert_eq!(n.out.dropped, 0, "a well-behaved node lost frames");
    }
    // Budget respected: never more than the largest budget (or one unit) + BusDone.
    for n in &s.nodes {
        assert!(n.max_answer <= MAX_BUDGET.max(MAX_UNIT) + MAX_DONE);
    }
    // Each answer of the flooder is bounded by the 4-node budget plus one frame.
    assert!(s.nodes[0].max_answer <= usize::from(s.master.budget()) + link::MAX_FRAME + MAX_DONE);
    s.check_order().unwrap();
    assert_eq!(s.undecodable, 0);
}

#[test]
fn slow_node_spikes_never_collide() {
    // A node that now and then reacts 3-9 ms late (a busy executor).
    let mut s = Sim::new(4, Traffic::busy(), 61);
    for n in s.nodes.iter_mut() {
        n.spike = (0.02, 6 * MS);
    }
    settle(&mut s);
    s.run(8 * SEC);
    for n in s.nodes.iter_mut() {
        n.traffic = Traffic::default();
        n.spike = (0.0, 0);
    }
    s.run(2 * SEC);
    let stale: u32 = s.nodes.iter().map(|n| n.node.counters().stale).sum();
    println!("2% of reactions 6 ms late: {stale} stale polls not answered, {} line collisions", s.collisions);
    assert_accounted(&s);
}

#[test]
fn assignments_reach_the_named_node_over_the_bus() {
    let mut s = Sim::new(3, Traffic::quiet(), 71);
    settle(&mut s);
    let a = Assignment { role: NodeRole::Wifi5, plan: heapless::Vec::new(), id: 42 };
    let mut b = cmd(&Command::To { node: mac_of(1) });
    b.extend(cmd(&Command::Assign(a)));
    s.hub_send(&b);
    s.run(100 * MS);
    let got: Vec<u32> = s.nodes.iter().map(|n| n.assigns).collect();
    assert_eq!(got, [0, 1, 0]);
}

/// Latency and throughput at 921600 with realistic frame mixes, for the
/// docs. A frame's latency runs from the moment it is queued on the node to
/// the moment the hub has checked its answer.
#[test]
fn latency_and_throughput_table() {
    println!();
    println!("| nodes | traffic | frames/s | bytes/s on return wire | line busy | mean | p50 | p99 | max | budget |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for (name, traffic) in [("quiet", Traffic::quiet()), ("busy", Traffic::busy())] {
        for n in [2usize, 4, 8] {
            let mut s = Sim::new(n, traffic.clone(), 100 + n as u64);
            settle(&mut s);
            s.run(SEC);
            let mark = s.delivered.len();
            let (busy0, bytes0, t0) = (s.master.stats.busy_us, s.ret_bytes, s.now);
            let secs = 10;
            s.run(secs * SEC);
            let (mean, p50, p99, max) = s.latency(mark);
            let frames = (s.delivered.len() - mark) as u64 / secs;
            let bytes = (s.ret_bytes - bytes0) / secs;
            let busy = (s.master.stats.busy_us - busy0) as f64 / ((s.now - t0) / US) as f64 * 100.0;
            let ms = |x: u64| x as f64 / MS as f64;
            println!(
                "| {n} | {name} | {frames} | {bytes} | {busy:.0}% | {:.1} ms | {:.1} ms | {:.1} ms | {:.1} ms | {} |",
                ms(mean),
                ms(p50),
                ms(p99),
                ms(max),
                s.master.budget()
            );
            assert!(p99 < 45 * MS, "{n} {name}: p99 {:.1} ms", ms(p99));
            assert!(mean < 25 * MS);
            for nd in &s.nodes {
                assert_eq!(nd.out.dropped, 0);
            }
            s.check_order().unwrap();
        }
    }
    // Headroom: the most one bus moves, 8 nodes flooding.
    let mut s = Sim::new(8, Traffic::flood(2000.0), 9);
    settle(&mut s);
    let mark = s.delivered.len();
    let b0 = s.ret_bytes;
    s.run(5 * SEC);
    println!(
        "8 nodes all flooding: {} frames/s, {} bytes/s on the return wire ({:.0}% of the line's 92160)",
        (s.delivered.len() - mark) / 5,
        (s.ret_bytes - b0) / 5,
        (s.ret_bytes - b0) as f64 / 5.0 / 921.6
    );
}

#[test]
fn polls_and_overheads() {
    // Sizes the timing budget in docs/WIREBUS.md uses.
    let poll = cmd(&Command::Poll { addr: 8, epoch: 0xBEEF, ack: 200, budget: 1024 });
    let slot = cmd(&Command::JoinSlot { epoch: 0xBEEF, slots: 8, slot_us: 600 });
    let grant = cmd(&Command::Address { mac: [0xFF; 6], addr: 8, epoch: 0xBEEF, check: 0xFFFF });
    let mut b = [0u8; 64];
    let done = link::encode(&Frame::BusDone { addr: 8, seq: 200, more: true, crc: 0xFFFF }, &mut b).unwrap().len();
    let join = link::encode(&Frame::BusJoin { mac: [0xFF; 6], want: 8, check: 0xFFFF }, &mut b).unwrap().len();
    let mut rng = crate::sim::Rng::new(1);
    let mut sizes = Vec::new();
    for k in [Kind::Stats, Kind::RidSingle, Kind::Threat, Kind::DroneId, Kind::RidPack, Kind::RidFull] {
        let mut fb = [0u8; link::MAX_FRAME];
        sizes.push((k, link::encode(&make_frame(k, 123_456_789, &mut rng), &mut fb).unwrap().len()));
    }
    println!("poll {} B, join slot {} B, grant {} B, BusDone {done} B, BusJoin {join} B; frames {sizes:?}", poll.len(), slot.len(), grant.len());
    assert!(poll.len() <= 10 && done <= 12);
}

/// A node that puts `Via { victim }` before its frames cannot attribute them
/// to the victim: the Via is refused and the frame goes under its own MAC.
#[test]
fn a_node_cannot_borrow_a_victims_mac() {
    let mut b = [0u8; 64];
    let (me, victim) = ([1; 6], [9; 6]);
    let via = link::encode(&Frame::Via { node: victim, rssi: -40 }, &mut b).unwrap().to_vec();
    let f1 = link::encode(&stats_frame(1), &mut b).unwrap().to_vec();
    let mut a = Attributor::new();
    let mut stream = Vec::new();
    for chunk in [&via, &f1, &via] {
        a.frame(me, chunk, &mut |x| stream.extend_from_slice(x));
    }
    a.end_answer();
    let mut got = Vec::new();
    link::Decoder::new().feed(&stream, |r| got.push(r.unwrap()));
    assert_eq!(got, [Frame::Via { node: me, rssi: 0 }, stats_frame(1)]);
    assert_eq!((a.refused, a.relayed), (2, 0));
    let mut reports = Vec::new();
    a.log.drain(1000, 1000, |r| reports.push(r));
    assert_eq!(reports.len(), 1);
    assert_eq!((reports[0].source, reports[0].detail, reports[0].count), (me, link::origin::FORGED_ORIGIN, 2));
}

/// A registered gateway's Via still names the node it relays.
#[test]
fn a_registered_gateway_still_can() {
    let mut b = [0u8; 64];
    let (gw, relayed) = ([1; 6], [9; 6]);
    let via = link::encode(&Frame::Via { node: relayed, rssi: -40 }, &mut b).unwrap().to_vec();
    let f1 = link::encode(&stats_frame(1), &mut b).unwrap().to_vec();
    let mut a = Attributor::new();
    a.register_gateway(gw);
    let mut stream = Vec::new();
    for chunk in [&via, &f1] {
        a.frame(gw, chunk, &mut |x| stream.extend_from_slice(x));
    }
    let mut got = Vec::new();
    link::Decoder::new().feed(&stream, |r| got.push(r.unwrap()));
    assert_eq!(got, [Frame::Via { node: relayed, rssi: -40 }, stats_frame(1)]);
    assert_eq!((a.refused, a.relayed), (0, 1));
    // ...but another node on the same bus still cannot.
    let other = [3; 6];
    let mut n = 0;
    a.frame(other, &via, &mut |_| n += 1);
    assert_eq!((n, a.refused), (0, 1));
}

/// Frames only the hub, the gateway or the bus layer may originate are
/// refused from a bus node; a registered gateway may send its own frames
/// (no Via, as on a gateway link) but may not relay them for a node.
#[test]
fn bus_nodes_cannot_send_hub_or_gateway_frames() {
    let mut b = [0u8; 400];
    let me = [1; 6];
    let fix = Frame::HubFix(link::HubFix { state: link::GnssState::Fix3d, sats: Some(9), position: None, utc_ms: Some(1), utc_pps: true });
    let csbee = Frame::Csbee(b"#A:x".iter().copied().collect());
    let flags = Frame::Flags(link::FlagReport { who: "x".try_into().unwrap(), level: 3, items: heapless::Vec::new(), pack_date: 0 });
    let join = Frame::BusJoin { mac: [5; 6], want: 0, check: 0 };
    let gw_hello = Frame::Hello { role: NodeRole::Gateway, protocol: 13, firmware: "gw key x".try_into().unwrap() };
    let mut a = Attributor::new();
    for f in [&fix, &csbee, &flags, &join, &gw_hello] {
        let bytes = link::encode(f, &mut b).unwrap().to_vec();
        a.frame(me, &bytes, &mut |_| panic!("{f:?} passed"));
    }
    assert_eq!(a.refused, 5);
    // A registered gateway: its own Hello passes bare; relaying a HubFix does not.
    a.register_gateway(me);
    let hello = link::encode(&gw_hello, &mut b).unwrap().to_vec();
    let mut stream = Vec::new();
    a.frame(me, &hello, &mut |x| stream.extend_from_slice(x));
    let via = link::encode(&Frame::Via { node: [9; 6], rssi: -40 }, &mut b).unwrap().to_vec();
    let fixb = link::encode(&fix, &mut b).unwrap().to_vec();
    a.frame(me, &via, &mut |x| stream.extend_from_slice(x));
    a.frame(me, &fixb, &mut |x| stream.extend_from_slice(x));
    a.frame(me, &fixb, &mut |x| stream.extend_from_slice(x));
    let mut got = Vec::new();
    link::Decoder::new().feed(&stream, |r| got.push(r.unwrap()));
    assert_eq!(got, [gw_hello]);
    assert_eq!(a.refused, 7);
}
