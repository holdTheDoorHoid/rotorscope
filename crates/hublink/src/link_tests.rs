//! Host tests for the hub verifier (docs/LINK-KEYS-DESIGN.md section 9, the
//! "hub verifier" rows, and section 13 "Hub (13e) must"). A real
//! `airlink::v3::Node` plays each node; the gateway is these tests feeding
//! `GwUp` messages and reading back what the hub queued. Fixed, obvious test
//! keys only.

use std::vec::Vec as StdVec;

use super::*;
use airlink::v3::{Node, NodeKeys, Verdict};
use kitkeys::NodeKey;
use link::gw::GwDecoder;

const KA: [u8; 32] = [0x11; 32];
const KB: [u8; 32] = [0x22; 32];
const MAC_A: Mac = [0x02, 0, 0, 0, 0, 0x0a];
const MAC_B: Mac = [0x02, 0, 0, 0, 0, 0x0b];
const MAC_X: Mac = [0x02, 0, 0, 0, 0, 0x77];
const GW: Mac = [0x02, 0, 0, 0, 0, 0x99];

fn rng() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0x5000);
    N.fetch_add(1, Ordering::Relaxed)
}

/// What the hub queued for the gateway, owned.
#[derive(Debug, Clone, PartialEq)]
enum Down {
    Roster(StdVec<Mac>),
    Beacon(StdVec<u8>),
    Transmit { id: u16, to: Mac, urgent: bool, give_up_ms: u16, data: StdVec<u8> },
    Cancel(Mac),
}

/// The UART queue: every message the hub sent, decoded as the gateway would.
#[derive(Default)]
struct Wire {
    sent: StdVec<Down>,
    full: bool,
}

impl Sink for Wire {
    fn send(&mut self, bytes: &[u8]) -> bool {
        if self.full {
            return false;
        }
        let mut store = [0u8; MAX_GW_FRAME];
        let mut dec = GwDecoder::new(&mut store);
        let mut got = StdVec::new();
        dec.feed_down(bytes, |r| {
            got.push(match r.expect("the hub sends only valid messages") {
                GwDown::Roster(v) => Down::Roster(v.to_vec()),
                GwDown::Beacon(d) => Down::Beacon(d.to_vec()),
                GwDown::Transmit { id, to, urgent, give_up_ms, data } => Down::Transmit { id, to, urgent, give_up_ms, data: data.to_vec() },
                GwDown::Cancel { to } => Down::Cancel(to),
            })
        });
        assert_eq!(got.len(), 1, "one whole message per send");
        self.sent.extend(got);
        true
    }
}

impl Wire {
    fn take(&mut self) -> StdVec<Down> {
        core::mem::take(&mut self.sent)
    }
}

/// Owned copy of what `on_up` hands out.
#[derive(Debug, Clone, PartialEq)]
enum Got {
    Batch { slot: Option<u8>, mac: Mac, payload: StdVec<u8> },
    Gateway { accepted: bool, protocol: u8 },
    Refused { source: Mac, detail: u32, count: u32 },
}

struct Kit {
    link: Box<HubLink>,
    wire: Wire,
    /// Hub uptime, ms.
    ht: u64,
    nodes: StdVec<(Node, Mac)>,
    /// Node uptime, ms (one clock for both nodes: they never compare).
    nt: u64,
    seq: u16,
}

fn keyed(epoch: u32) -> Box<HubLink> {
    let mut l = Box::new(HubLink::closed());
    let hub = l.start_keyed(epoch);
    assert!(hub.set_slot(0, MAC_A, &NodeKey::from_bytes(KA)));
    assert!(hub.set_slot(1, MAC_B, &NodeKey::from_bytes(KB)));
    l.slots_loaded([(0, MAC_A), (1, MAC_B)]);
    l
}

fn hello(protocol: u8) -> GwUp<'static> {
    GwUp::Hello { protocol, firmware: "gateway test".try_into().unwrap(), mac: GW, channel: 6 }
}

impl Kit {
    fn new(link: Box<HubLink>) -> Self {
        let nodes = std::vec![
            (Node::new(NodeKeys::new(0, MAC_A, &NodeKey::from_bytes(KA)), rng), MAC_A),
            (Node::new(NodeKeys::new(1, MAC_B, &NodeKey::from_bytes(KB)), rng), MAC_B),
        ];
        Kit { link, wire: Wire::default(), ht: 1_000_000, nodes, nt: 20_000, seq: 0 }
    }

    fn up(&mut self, m: GwUp<'_>) -> StdVec<Got> {
        let mut got = StdVec::new();
        self.link.on_up(m, self.ht, &mut self.wire, &mut |u| {
            got.push(match u {
                Up::Batch { slot, mac, payload, .. } => Got::Batch { slot, mac, payload: payload.to_vec() },
                Up::Gateway { accepted, protocol } => Got::Gateway { accepted, protocol },
                Up::Refused(r) => Got::Refused { source: r.source, detail: r.detail, count: r.count },
            })
        });
        got
    }

    fn packet(&mut self, src: Mac, data: &[u8]) -> StdVec<Got> {
        self.up(GwUp::Packet { src, rssi: -50, held_us: 100, data })
    }

    fn tick(&mut self) -> StdVec<Refusal> {
        let mut r = StdVec::new();
        self.link.tick(self.ht, &mut self.wire, |x| r.push(x));
        r
    }

    fn wait(&mut self, ms: u64) {
        self.ht += ms;
        self.nt += ms;
    }

    fn beacons(&mut self) -> StdVec<StdVec<u8>> {
        self.wire.take().into_iter().filter_map(|d| if let Down::Beacon(b) = d { Some(b) } else { None }).collect()
    }

    /// Node `i` hears `p` from the gateway.
    fn hears(&mut self, i: usize, p: &[u8], timesync: bool) -> Verdict {
        self.nodes[i].0.accept(&GW, p, self.nt, timesync).verdict()
    }

    /// Node `i` seals a batch of one frame.
    fn batch(&mut self, i: usize) -> StdVec<u8> {
        let mut b = airlink::Batch::new();
        assert!(b.push(&[1, 2, 3, 0]));
        self.seq = self.seq.wrapping_add(1);
        b.sealed_v3(self.seq, &mut self.nodes[i].0, self.nt).expect("node has an epoch").to_vec()
    }

    /// Gateway up, a beacon out and heard by both nodes.
    fn start(&mut self) {
        assert_eq!(self.up(hello(14)), [Got::Gateway { accepted: true, protocol: 14 }]);
        self.tick();
        let b = self.beacons();
        assert_eq!(b.len(), 1);
        assert_eq!(self.hears(0, &b[0], false), Verdict::Accept);
        assert_eq!(self.hears(1, &b[0], false), Verdict::Accept);
    }
}

fn transmits(d: &[Down]) -> StdVec<(u16, Mac, bool, u16, StdVec<u8>)> {
    d.iter()
        .filter_map(|x| match x {
            Down::Transmit { id, to, urgent, give_up_ms, data } => Some((*id, *to, *urgent, *give_up_ms, data.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn gateway_hello_13_refused() {
    let mut k = Kit::new(keyed(100));
    assert_eq!(k.up(hello(13)), [Got::Gateway { accepted: false, protocol: 13 }]);
    k.tick();
    k.wait(600);
    k.tick();
    assert!(k.wire.take().is_empty(), "no roster, no beacon for an old gateway");
    assert!(k.link.view(k.ht).old_gateway);
    // Nothing can be sealed without the gateway's address.
    assert!(matches!(k.link.command(MAC_A, &[4, 1, 0], k.ht, &mut k.wire), Err(CommandError::NoGateway)));
    // A reflashed gateway is taken, and the strip clears.
    assert_eq!(k.up(hello(14)), [Got::Gateway { accepted: true, protocol: 14 }]);
    assert!(!k.link.view(k.ht).old_gateway);
    assert_eq!(k.wire.take(), [Down::Roster(std::vec![MAC_A, MAC_B])]);
}

#[test]
fn roster_and_beacons_on_schedule() {
    let mut k = Kit::new(keyed(100));
    k.tick();
    assert!(k.wire.take().is_empty(), "nothing before the gateway's Hello");
    k.up(hello(14));
    k.tick();
    let d = k.wire.take();
    assert_eq!(d[0], Down::Roster(std::vec![MAC_A, MAC_B]));
    assert!(matches!(d[1], Down::Beacon(_)));
    // Beacons every 500 ms, roster every 5 s.
    let (mut beacons, mut rosters) = (0, 0);
    for _ in 0..100 {
        k.wait(100);
        k.tick();
        for x in k.wire.take() {
            match x {
                Down::Beacon(b) => {
                    beacons += 1;
                    assert_eq!(b.len(), v3::beacon_len(2));
                    // Each node finds its own tag good.
                    assert_eq!(k.hears(0, &b, false), Verdict::Accept);
                    assert_eq!(k.hears(1, &b, false), Verdict::Accept);
                }
                Down::Roster(_) => rosters += 1,
                other => panic!("{other:?}"),
            }
        }
    }
    assert_eq!(beacons, 20);
    assert_eq!(rosters, 2);
    // Before the SD mount: a beacon now if the last is 250 ms old.
    k.wait(300);
    k.link.beacon_if_older(k.ht, 250, &mut k.wire);
    assert_eq!(k.beacons().len(), 1);
    k.link.beacon_if_older(k.ht, 250, &mut k.wire);
    assert!(k.beacons().is_empty());
}

#[test]
fn verified_batch_goes_to_its_slot_then_sync_then_time_sync() {
    let mut k = Kit::new(keyed(100));
    k.start();
    let p = k.batch(0);
    let got = k.packet(MAC_A, &p);
    assert_eq!(got, [Got::Batch { slot: Some(0), mac: MAC_A, payload: std::vec![1, 2, 3, 0] }]);
    assert_eq!(HubLink::node_number(0), 1);
    // A first Sync for the new session, unicast to A, echoing its batch.
    let t = transmits(&k.wire.take());
    assert_eq!(t.len(), 1);
    let (sync_id, to, urgent, give_up, data) = t[0].clone();
    assert_eq!((to, urgent, give_up, data.len()), (MAC_A, false, COMMAND_GIVE_UP_MS, v3::SYNC_LEN));
    assert_eq!(k.hears(0, &data, false), Verdict::Accept);
    assert!(k.nodes[0].0.proven());
    // No time sync until the gateway says the Sync was acknowledged.
    k.link.time_sync_round(k.ht);
    assert!(!k.link.time_sync_pending());
    k.up(GwUp::TxDone { id: sync_id, to: MAC_A, radio_acked: true, attempts: 1, held_ms: 3 });
    k.link.time_sync_round(k.ht);
    assert!(k.link.time_sync_pending());
    assert!(k.link.time_sync_step(k.ht, 1_763_400_000_123, true, &mut k.wire));
    assert!(!k.link.time_sync_step(k.ht, 1_763_400_000_123, true, &mut k.wire), "B never synced: one only");
    let t = transmits(&k.wire.take());
    assert_eq!(t.len(), 1);
    let (_, to, urgent, give_up, data) = t[0].clone();
    assert_eq!((to, urgent, give_up), (MAC_A, true, TIMESYNC_GIVE_UP_MS));
    // Alone in its own Command, and the node takes it under the strict rule.
    let shape = v3::inspect(&data).unwrap();
    assert_eq!(shape.kind, Kind::Command);
    let frames: StdVec<&[u8]> = v3::frames(v3::payload(&data, &shape)).collect();
    assert_eq!(frames.len(), 1);
    let mut f = frames[0].to_vec();
    assert_eq!(postcard::from_bytes_cobs::<link::Command>(&mut f).unwrap(), link::Command::TimeSync { utc_ms: 1_763_400_000_123, pps: true });
    assert_eq!(k.hears(0, &data, true), Verdict::Accept);
}

#[test]
fn a_sync_not_acknowledged_is_sent_again() {
    let mut k = Kit::new(keyed(100));
    k.start();
    let p = k.batch(0);
    k.packet(MAC_A, &p);
    let t = transmits(&k.wire.take());
    k.up(GwUp::TxDone { id: t[0].0, to: MAC_A, radio_acked: false, attempts: 9, held_ms: 2000 });
    k.wait(1000);
    k.tick();
    assert!(transmits(&k.wire.take()).is_empty(), "not before the retry time");
    k.wait(1100);
    k.tick();
    let t = transmits(&k.wire.take());
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].4.len(), v3::SYNC_LEN);
    // Once acknowledged, no more.
    k.up(GwUp::TxDone { id: t[0].0, to: MAC_A, radio_acked: true, attempts: 1, held_ms: 2 });
    k.wait(5000);
    k.tick();
    assert!(transmits(&k.wire.take()).is_empty());
}

#[test]
fn slot_key_bound_to_mac() {
    // A captured node (A's key) claims B's address: refused as a bad tag
    // for B's slot, nothing handed on, B unaffected.
    let mut k = Kit::new(keyed(100));
    k.start();
    let forged = k.batch(0);
    assert!(k.packet(MAC_B, &forged).is_empty());
    assert_eq!(k.link.stats.rejects[Reject::BadTag.index()], 1);
    let r = k.tick();
    k.wait(1100);
    let r: StdVec<_> = r.into_iter().chain(k.tick()).collect();
    assert_eq!(r.len(), 1);
    assert_eq!((r[0].source, r[0].detail, r[0].count), (MAC_B, 1, 1));
    let good = k.batch(1);
    assert_eq!(k.packet(MAC_B, &good).len(), 1);
}

#[test]
fn gateway_has_no_key_and_cannot_forge() {
    let mut k = Kit::new(keyed(100));
    k.start();
    // Altered in transit: refused.
    let mut p = k.batch(0);
    p[airlink::HEADER] ^= 0x40;
    assert!(k.packet(MAC_A, &p).is_empty());
    assert_eq!(k.link.stats.rejects[Reject::BadTag.index()], 1);
    // Replayed: the second copy of an accepted batch is a duplicate (quiet),
    // an older one a replay.
    let p1 = k.batch(0);
    let p2 = k.batch(0);
    assert_eq!(k.packet(MAC_A, &p2).len(), 1);
    assert!(k.packet(MAC_A, &p2).is_empty());
    assert_eq!(k.link.stats.duplicates, 1);
    assert!(k.packet(MAC_A, &p1).is_empty());
    assert_eq!(k.link.stats.rejects[Reject::Replay.index()], 1);
    // An unknown address: not in the roster.
    assert!(k.packet(MAC_X, &p2).is_empty());
    assert_eq!(k.link.stats.rejects[Reject::NotInRoster.index()], 1);
}

#[test]
fn altered_command_rejected_at_node() {
    let mut k = Kit::new(keyed(100));
    k.start();
    // Not heard yet this hub boot: nothing to echo, refused.
    assert_eq!(k.link.command(MAC_A, &[4, 1, 2, 0], k.ht, &mut k.wire), Err(CommandError::Seal(v3::SealError::NotHeard)));
    let p = k.batch(0);
    k.packet(MAC_A, &p);
    k.wire.take();
    k.link.command(MAC_A, &[4, 1, 2, 0], k.ht, &mut k.wire).unwrap();
    let t = transmits(&k.wire.take());
    let (id, to, urgent, give_up, mut data) = t[0].clone();
    assert_eq!((to, urgent, give_up), (MAC_A, false, COMMAND_GIVE_UP_MS));
    // The gateway flips a bit: the node refuses it; the original is taken.
    let good = data.clone();
    data[airlink::HEADER] ^= 1;
    assert_eq!(k.hears(0, &data, false), Verdict::Reject(Reject::BadTag));
    assert_eq!(k.hears(0, &good, false), Verdict::Accept);
    // Node B cannot take A's command (its own key, its own MAC).
    assert_ne!(k.hears(1, &good, false), Verdict::Accept);
    // A newer command while the first is still queued: cancel it first.
    k.link.command(MAC_A, &[4, 9, 0], k.ht, &mut k.wire).unwrap();
    let d = k.wire.take();
    assert_eq!(d[0], Down::Cancel(MAC_A));
    assert!(matches!(d[1], Down::Transmit { .. }));
    // Once the gateway reports it done, no cancel.
    let id2 = transmits(&d)[0].0;
    assert_ne!(id, id2);
    k.up(GwUp::TxDone { id: id2, to: MAC_A, radio_acked: true, attempts: 1, held_ms: 4 });
    k.link.command(MAC_A, &[4, 8, 0], k.ht, &mut k.wire).unwrap();
    assert!(matches!(k.wire.take()[..], [Down::Transmit { .. }]));
    // Not whole frames, or not a node of this kit.
    assert!(k.link.command(MAC_A, &[4, 8], k.ht, &mut k.wire).is_err());
    assert_eq!(k.link.command(MAC_X, &[4, 8, 0], k.ht, &mut k.wire), Err(CommandError::NotInRoster));
}

#[test]
fn genuine_stale_batch_is_answered_with_a_rate_limited_sync() {
    // The node follows an earlier hub boot (epoch 99); the hub is now 100.
    let mut old = Kit::new(keyed(99));
    old.start();
    let mut k = Kit::new(keyed(100));
    k.nodes = core::mem::take(&mut old.nodes);
    k.nt = old.nt;
    k.up(hello(14));
    k.wire.take();
    let p = k.batch(0);
    assert!(k.packet(MAC_A, &p).is_empty());
    assert_eq!(k.link.stats.rejects[Reject::Stale.index()], 1);
    let t = transmits(&k.wire.take());
    assert_eq!(t.len(), 1, "one Sync, echoing the refused batch");
    assert_eq!(k.hears(0, &t[0].4, false), Verdict::Accept, "the Sync moves the node to epoch 100");
    assert_eq!(k.nodes[0].0.epoch(), Some(100));
    // A forged stale batch (bad tag) gets no Sync.
    let mut f = k.batch(0);
    f[airlink::HEADER] ^= 2;
    k.packet(MAC_A, &f);
    // And answers are limited to one a second per slot: a second genuine
    // stale batch at once gets none.
    let mut old2 = Kit::new(keyed(98));
    old2.start();
    k.nodes[1] = core::mem::take(&mut old2.nodes).remove(1);
    let s1 = k.batch(1);
    let s2 = k.batch(1);
    k.packet(MAC_B, &s1);
    k.packet(MAC_B, &s2);
    assert_eq!(transmits(&k.wire.take()).len(), 1);
    assert_eq!(k.link.stats.sync_answers, 2);
}

#[test]
fn withheld_batch_late_at_hub_and_answered() {
    let mut k = Kit::new(keyed(100));
    k.start();
    let p = k.batch(0);
    k.ht += 2_500; // held back by the gateway
    assert!(k.packet(MAC_A, &p).is_empty());
    assert_eq!(k.link.stats.rejects[Reject::LateUplink.index()], 1);
    // Genuine: answered with a Sync (its clock estimate may be off).
    assert_eq!(transmits(&k.wire.take()).len(), 1);
}

#[test]
fn unprovisioned_and_old_firmware_are_counted_for_the_screen() {
    let mut k = Kit::new(keyed(100));
    k.start();
    let mut b = [0u8; v3::UNPROVISIONED_LEN];
    for (i, mac) in [MAC_X, [2, 0, 0, 0, 1, 1], MAC_X].iter().enumerate() {
        let a = v3::Unprovisioned { mac: *mac, image: 1, firmware: [b'n'; 16] };
        let p = v3::unprovisioned(&mut b, i as u16, &a).unwrap().to_vec();
        assert!(k.packet(*mac, &p).is_empty());
    }
    assert_eq!(k.link.view(k.ht).unprovisioned, 2);
    // A version 2 (old firmware) packet, and the gateway's own report.
    let mut v2 = std::vec![0u8; 60];
    v2[..4].copy_from_slice(&[b'R', b'S', 2, 2]);
    k.packet([2, 0, 0, 0, 2, 2], &v2);
    assert_eq!(k.link.stats.rejects[Reject::OldFirmware.index()], 1);
    let got = k.up(GwUp::Refused { src: [2, 0, 0, 0, 3, 3], reason: RefuseReason::OldFirmware, count: 4, rssi: -70, window_ms: 1000 });
    assert_eq!(got, [Got::Refused { source: [2, 0, 0, 0, 3, 3], detail: 8, count: 4 }]);
    assert_eq!(k.link.view(k.ht).old_nodes, 2);
    // All of it ages out after a minute.
    k.wait(SEEN_MS);
    assert_eq!(k.link.view(k.ht), View::default());
}

#[test]
fn gateway_refusals_become_forged_link_details() {
    let mut k = Kit::new(keyed(100));
    for (reason, detail) in [
        (RefuseReason::NotInRoster, 9),
        (RefuseReason::OverBudget, 10),
        (RefuseReason::OldFirmware, 8),
        (RefuseReason::Malformed, 15),
    ] {
        let got = k.up(GwUp::Refused { src: MAC_X, reason, count: 3, rssi: -60, window_ms: 1000 });
        assert_eq!(got, [Got::Refused { source: MAC_X, detail, count: 3 }]);
    }
    assert_eq!(k.link.stats.gw_refused, [3; 4]);
}

#[test]
fn hub_refusals_are_reported_once_a_second_with_their_codes() {
    let mut k = Kit::new(keyed(100));
    k.start();
    let mut v1 = airlink::Batch::new();
    assert!(v1.push(&[1, 0]));
    let v1 = v1.packet(1).to_vec();
    for _ in 0..5 {
        k.packet(MAC_A, &v1); // unauthenticated at a keyed hub
    }
    k.packet(MAC_X, &v1); // the version is checked before the roster
    k.packet(MAC_X, b"not ours"); // foreign: counted, never a threat
    k.wait(1000);
    let mut r = k.tick();
    r.sort_by_key(|x| x.detail);
    let got: StdVec<(Mac, u32, u32)> = r.iter().map(|x| (x.source, x.detail, x.count)).collect();
    assert_eq!(got, [(MAC_A, 3, 5), (MAC_X, 3, 1)]);
    assert_eq!(k.link.stats.rejects[Reject::Foreign.index()], 1);
    // Nothing more until something new.
    k.wait(1000);
    assert!(k.tick().is_empty());
}

#[test]
fn open_kit_takes_version_1_and_refuses_keyed_nodes() {
    let mut l = Box::new(HubLink::closed());
    l.open();
    let mut k = Kit::new(l);
    k.up(hello(14));
    k.tick();
    let d = k.wire.take();
    assert_eq!(d[0], Down::Roster(StdVec::new()), "an open kit has no roster");
    let Down::Beacon(b) = &d[1] else { panic!("{d:?}") };
    let p = airlink::parse(b).unwrap();
    assert_eq!((p.kind, p.sealed, airlink::beacon_channel(&p)), (airlink::Kind::Beacon, false, Some(6)));
    let mut v1 = airlink::Batch::new();
    assert!(v1.push(&[5, 6, 0]));
    let v1 = v1.packet(1).to_vec();
    assert_eq!(k.packet(MAC_X, &v1), [Got::Batch { slot: None, mac: MAC_X, payload: std::vec![5, 6, 0] }]);
    // A keyed node (version 3) at an open hub: refused, not taken unchecked.
    let mut keyed_hub = Kit::new(keyed(5));
    keyed_hub.start();
    let v3p = keyed_hub.batch(0);
    assert!(k.packet(MAC_A, &v3p).is_empty());
    assert_eq!(k.link.stats.rejects[Reject::NotInRoster.index()], 1);
    // Commands and time syncs go unsealed, to whoever reported.
    k.link.command(MAC_X, &[4, 1, 0], k.ht, &mut k.wire).unwrap();
    k.link.time_sync_round(k.ht);
    assert!(k.link.time_sync_step(k.ht, 5, false, &mut k.wire));
    let t = transmits(&k.wire.take());
    assert_eq!(t.len(), 2);
    for (_, to, _, _, data) in &t {
        assert_eq!(*to, MAC_X);
        let p = airlink::parse(data).unwrap();
        assert_eq!((p.kind, p.sealed), (airlink::Kind::Command, false));
    }
}

#[test]
fn closed_kit_sends_nothing_and_takes_nothing() {
    let mut k = Kit::new(Box::new(HubLink::closed()));
    k.up(hello(14));
    k.tick();
    k.wait(1000);
    k.tick();
    assert_eq!(k.wire.take(), [Down::Roster(StdVec::new())], "an empty roster, and no beacons");
    let mut v1 = airlink::Batch::new();
    assert!(v1.push(&[5, 6, 0]));
    assert!(k.packet(MAC_X, v1.packet(1)).is_empty());
    let mut other = Kit::new(keyed(5));
    other.start();
    let p = other.batch(0);
    assert!(k.packet(MAC_A, &p).is_empty());
    assert_eq!(k.link.stats.rejects[Reject::NotInRoster.index()], 2);
    assert_eq!(k.link.command(MAC_A, &[4, 0], k.ht, &mut k.wire), Err(CommandError::Closed));
    k.link.time_sync_round(k.ht);
    assert!(!k.link.time_sync_pending());
}

#[test]
fn a_full_queue_costs_the_message_not_the_state() {
    let mut k = Kit::new(keyed(100));
    k.up(hello(14));
    k.wire.full = true;
    k.tick();
    assert!(k.link.stats.sink_full >= 1);
    k.wire.full = false;
    k.tick();
    // The beacon and roster go out on the next pass.
    let d = k.wire.take();
    assert!(d.iter().any(|x| matches!(x, Down::Beacon(_))) && d.iter().any(|x| matches!(x, Down::Roster(_))));
}

#[test]
fn empty_roster_makes_no_beacon() {
    let mut l = Box::new(HubLink::closed());
    l.start_keyed(7);
    l.slots_loaded([]);
    let mut k = Kit::new(l);
    k.up(hello(14));
    k.tick();
    assert_eq!(k.wire.take(), [Down::Roster(StdVec::new())]);
}

/// Sizes the firmware budgets for (it keeps `HubLink` in PSRAM).
#[test]
fn sizes() {
    let n = core::mem::size_of::<HubLink>();
    std::println!("HubLink {n} bytes");
    assert!(n < 16 * 1024, "{n}");
}
