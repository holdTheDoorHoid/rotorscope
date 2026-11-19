//! Fuzz-style tests (crates/fuzzkit) for what the hub reads from outside:
//! the partition table, the `rskit` and `rsboot` partitions through
//! `read_kit` and `advance_boot_counter`, the gateway's byte stream through
//! `HubLink::on_up`, and `split_to`. Fixed, obvious test keys only.

use std::vec::Vec as StdVec;

use airlink::v3::{self, Node, NodeKeys};
use kitkeys::{NodeKey, SLOTS, bootlog, hub};
use link::gw::{self, GwDecoder, GwUp, MAX_GW_FRAME};

use crate::kit::{self, Counter, KitRead};
use crate::link::{HubLink, Up, split_to};
use crate::parttable::{self, PartError, tests::hub_table};

const ITERS: usize = 2_000;

#[test]
fn fuzz_partition_table() {
    let t = hub_table();
    let short = parttable::tests::table(&[parttable::tests::entry("rskit", 1, 0x40, 0xC10000, 0x2000)]);
    let seeds: [&[u8]; 2] = [&t, &short];
    let cfg = fuzzkit::Config::new().max_len(parttable::TABLE_LEN);
    fuzzkit::run_with("hublink::parttable::data_partition", &cfg, &seeds, ITERS * 2, |bytes| {
        let n = parttable::entries(bytes).count();
        assert!(n <= bytes.len() / parttable::ENTRY_LEN);
        for (name, size) in [(parttable::RSKIT, hub::PARTITION_LEN as u32), (parttable::RSBOOT, bootlog::PARTITION_LEN as u32)] {
            match parttable::data_partition(bytes, name, size) {
                Ok(e) => {
                    // Accepted implies safe to write.
                    assert_eq!(e.name(), name.as_bytes());
                    assert_eq!(e.size, size);
                    assert_eq!(e.ptype, parttable::TYPE_DATA);
                    assert_eq!(e.offset % 0x1000, 0);
                    assert!(u64::from(e.offset) >= 0x9000);
                    let end = u64::from(e.offset) + u64::from(e.size);
                    for o in parttable::entries(bytes) {
                        if o == e {
                            continue;
                        }
                        let oend = u64::from(o.offset) + u64::from(o.size);
                        assert!(oend <= u64::from(e.offset) || end <= u64::from(o.offset), "{o:?} overlaps {e:?}");
                    }
                }
                Err(PartError::NoTable) => assert_eq!(n, 0),
                Err(_) => {}
            }
        }
    });
}

#[test]
fn fuzz_read_kit_and_boot_counter() {
    let m = kit::tests::master(7);
    let roster = kit::tests::roster_for(7, &[[2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 2]]);
    let image = kit::tests::keyed_image(&m, &roster);
    // Seeds: the master copy, the roster copy and a few boot log entries.
    let master = image[..hub::MASTER_LEN].to_vec();
    let rost = image[hub::ROSTER_SECTOR..hub::ROSTER_SECTOR + hub::ROSTER_LEN].to_vec();
    let mut boots = StdVec::new();
    for v in [100u32, 101, 102] {
        boots.extend_from_slice(&bootlog::entry(v));
    }
    let seeds: [&[u8]; 3] = [&master, &rost, &boots];
    fuzzkit::run("hublink::kit::read_kit", &seeds, ITERS, |bytes| {
        // The first byte picks where the rest lands: master copy A or B,
        // roster copy A or B, or the boot log.
        let (pick, body) = bytes.split_first().unwrap_or((&0, &[]));
        let mut rskit = image.clone();
        let mut rsboot = bootlog::seed_image(100).to_vec();
        let (dst, at): (&mut StdVec<u8>, usize) = match pick % 5 {
            0 => (&mut rskit, hub::MASTER_OFFSETS[0]),
            1 => (&mut rskit, hub::MASTER_OFFSETS[1]),
            2 => (&mut rskit, hub::ROSTER_SECTOR),
            3 => (&mut rskit, hub::ROSTER_SECTOR + hub::ROSTER_OFFSETS[1]),
            _ => (&mut rsboot, usize::from(*pick) * bootlog::ENTRY_LEN),
        };
        let n = body.len().min(dst.len() - at);
        dst[at..at + n].copy_from_slice(&body[..n]);
        let mut f = kit::tests::flash_with(&rskit, &rsboot);
        let mut buf = [0u8; hub::SECTOR_LEN];
        if let KitRead::Keyed { kit_id, mut source, roster } = kit::read_kit(&mut f, &hub_table(), &mut buf).unwrap() {
            assert!(buf.iter().all(|&b| b == 0), "the buffer that held the master is wiped");
            let mut h = v3::Hub::new(1);
            let st = kit::load_slots(&mut source, kit_id, &roster, &mut h).unwrap();
            // Only live roster entries become slots, each at its own address.
            if let (kit::RosterRead::Good(r), kit::KitState::Keyed { slots, .. }) = (&roster, st) {
                let live: StdVec<_> = r.entries().filter(|e| !e.revoked).collect();
                assert!(usize::from(slots) <= live.len());
                for e in r.entries().filter(|e| e.revoked) {
                    if !live.iter().any(|l| l.mac == e.mac) {
                        assert_eq!(h.slot_of(&e.mac), None, "a revoked node was loaded");
                    }
                }
            }
        }
        let mut lb = [0u8; bootlog::PARTITION_LEN];
        let before = match bootlog::read(&rsboot) {
            bootlog::State::Good { latest, .. } => Some(latest),
            _ => None,
        };
        match kit::advance_boot_counter(&mut f, &hub_table(), &mut lb, true).unwrap() {
            Counter::Written { value, .. } => assert_eq!(Some(value), before.map(|l| l + 1)),
            Counter::WriteFailed { .. } | Counter::Missing | Counter::Damaged | Counter::Exhausted => {}
            other => panic!("unexpected {other:?}"),
        }
    });
}

const KA: [u8; 32] = [0x11; 32];
const KB: [u8; 32] = [0x22; 32];
const MAC_A: [u8; 6] = [0x02, 0, 0, 0, 0, 0x0a];
const MAC_B: [u8; 6] = [0x02, 0, 0, 0, 0, 0x0b];
const GW: [u8; 6] = [0x02, 0, 0, 0, 0, 0x99];
const EPOCH: u32 = 9;
const NOW: u64 = 1_000_000;

fn fixed_rng() -> u32 {
    0x5000
}

fn keyed() -> Box<HubLink> {
    let mut l = Box::new(HubLink::closed());
    let hub = l.start_keyed(EPOCH);
    assert!(hub.set_slot(0, MAC_A, &NodeKey::from_bytes(KA)));
    assert!(hub.set_slot(1, MAC_B, &NodeKey::from_bytes(KB)));
    l.slots_loaded([(0, MAC_A), (1, MAC_B)]);
    l
}

/// The gateway stream as seeds: a Hello, a genuine sealed batch from node A
/// (its beacon came from a hub with the same keys, epoch and clock), an open
/// batch, a refusal report and stats.
fn up_seeds() -> StdVec<StdVec<u8>> {
    let mut h = v3::Hub::new(EPOCH);
    h.set_gateway(GW);
    assert!(h.set_slot(0, MAC_A, &NodeKey::from_bytes(KA)));
    assert!(h.set_slot(1, MAC_B, &NodeKey::from_bytes(KB)));
    let mut n = Node::new(NodeKeys::new(0, MAC_A, &NodeKey::from_bytes(KA)), fixed_rng);
    let mut b = [0u8; v3::BEACON_MAX];
    let len = h.seal_beacon(&mut b, 1, 6, NOW).unwrap();
    assert!(matches!(n.accept(&GW, &b[..len], 5_000, false), v3::NodeVerdict::Accept(_)));
    let mut batch = airlink::Batch::new();
    assert!(batch.push(&[3, 1, 2, 0]));
    let sealed = batch.sealed_v3(2, &mut n, 5_000).unwrap().to_vec();
    let mut ob = airlink::Batch::new();
    assert!(ob.push(&[2, 7, 0]));
    let open = ob.packet(3).to_vec();
    let msgs = [
        GwUp::Hello { protocol: link::PROTOCOL_VERSION, firmware: "gateway test".try_into().unwrap(), mac: GW, channel: 6 },
        GwUp::Packet { src: MAC_A, rssi: -40, held_us: 10, data: &sealed },
        GwUp::Packet { src: MAC_B, rssi: -40, held_us: 10, data: &open },
        GwUp::TxDone { id: 1, to: MAC_A, radio_acked: true, attempts: 1, held_ms: 3 },
        GwUp::Refused { src: MAC_B, reason: gw::RefuseReason::OldFirmware, count: 2, rssi: -70, window_ms: 1000 },
        GwUp::Stats(gw::GwStats::default()),
    ];
    let mut enc = [0u8; MAX_GW_FRAME];
    let each: StdVec<StdVec<u8>> = msgs.iter().map(|m| gw::encode_up(m, &mut enc).unwrap().to_vec()).collect();
    // A Hello then the sealed batch, as one stream.
    let mut both = each[0].clone();
    both.extend_from_slice(&each[1]);
    let mut out = each;
    out.push(both);
    out
}

#[test]
fn fuzz_gateway_stream_into_on_up() {
    let seeds = up_seeds();
    let refs: StdVec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let cfg = fuzzkit::Config::new().max_len(2400);
    fuzzkit::run_with("hublink::HubLink::on_up", &cfg, &refs, ITERS, |bytes| {
        let mut open = Box::new(HubLink::closed());
        open.open();
        for mut link in [keyed(), open] {
            // Whatever the hub sends must be whole, valid messages.
            let mut sink = |b: &[u8]| {
                let mut store = [0u8; MAX_GW_FRAME];
                let mut d = GwDecoder::new(&mut store);
                let mut n = 0;
                d.feed_down(b, |r| {
                    assert!(r.is_ok(), "the hub sent a message that does not decode");
                    n += 1;
                });
                assert_eq!(n, 1);
                true
            };
            let mut store = [0u8; MAX_GW_FRAME];
            let mut dec = GwDecoder::new(&mut store);
            let mut now = NOW;
            dec.feed_up(bytes, |r| {
                let Ok(m) = r else { return };
                link.on_up(m, now, &mut sink, &mut |u| {
                    if let Up::Batch { slot: Some(s), mac, .. } = u {
                        assert!(usize::from(s) < SLOTS);
                        assert_eq!(link_slot_mac(s), Some(mac), "a batch was credited to the wrong slot");
                    }
                });
                now += 7;
            });
            link.tick(now + 600, &mut sink, |_| {});
        }
    });
}

/// The test roster: slot 0 is node A, slot 1 node B.
fn link_slot_mac(slot: u8) -> Option<[u8; 6]> {
    match slot {
        0 => Some(MAC_A),
        1 => Some(MAC_B),
        _ => None,
    }
}

#[test]
fn fuzz_split_to() {
    let mut buf = [0u8; 64];
    let to = link::encode_command(&link::Command::To { node: MAC_A }, &mut buf).unwrap().to_vec();
    let mut seed = to.clone();
    seed.extend_from_slice(&[4, 1, 2, 3, 0]);
    let seeds: [&[u8]; 2] = [&to, &seed];
    fuzzkit::run("hublink::split_to", &seeds, ITERS * 2, |bytes| {
        if let Some((_, rest)) = split_to(bytes) {
            assert!(rest.len() < bytes.len());
            assert_eq!(rest, &bytes[bytes.len() - rest.len()..]);
        }
    });
}

/// The sealed seed reaches the deep path: a keyed hub hands it on.
#[test]
fn sealed_seed_is_accepted() {
    let seeds = up_seeds();
    let mut link = keyed();
    let mut sink = |_: &[u8]| true;
    let mut store = [0u8; MAX_GW_FRAME];
    let mut dec = GwDecoder::new(&mut store);
    let mut batches = 0;
    dec.feed_up(seeds.last().unwrap(), |r| {
        link.on_up(r.unwrap(), NOW, &mut sink, &mut |u| {
            if let Up::Batch { slot: Some(0), .. } = u {
                batches += 1;
            }
        })
    });
    assert_eq!(batches, 1);
}
