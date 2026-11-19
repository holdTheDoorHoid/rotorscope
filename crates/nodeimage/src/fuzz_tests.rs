//! Fuzz-style tests (crates/fuzzkit): the app descriptor reader, the job
//! record, the `rskey` partition-table search, and the node's receive path
//! (`uplink::Link::receive`, open and keyed with a fixed test key) and its
//! command decoder.

use std::vec::Vec;

use link::{Assignment, Command, NodeRole};

use crate::keystore::{self, Found};
use crate::{HEAD_LEN, JOB_MAX, StoredJob, decode_job, encode_job, image_of, project_name, uplink};

const ITERS: usize = 4_000;

fn head(name: &str) -> Vec<u8> {
    let mut h = std::vec![0u8; HEAD_LEN + 16];
    h[0] = crate::IMAGE_MAGIC;
    h[crate::APP_DESC_OFFSET..crate::APP_DESC_OFFSET + 4].copy_from_slice(&crate::APP_DESC_MAGIC.to_le_bytes());
    let at = crate::APP_DESC_OFFSET + 48;
    h[at..at + name.len()].copy_from_slice(name.as_bytes());
    h
}

#[test]
fn fuzz_app_descriptor() {
    let (a, b) = (head("node-wifi"), head("gateway"));
    let seeds: [&[u8]; 2] = [&a, &b];
    fuzzkit::run("nodeimage::project_name", &seeds, ITERS, |bytes| {
        if let Some(name) = project_name(bytes) {
            assert!(!name.is_empty() && name.len() <= 32 && !name.contains('\0'));
        }
        if let Some(i) = image_of(bytes) {
            assert_eq!(project_name(bytes), Some(i.project()));
        }
    });
}

#[test]
fn fuzz_job_record() {
    let big = Assignment { role: NodeRole::DroneId, plan: (0..24).map(|i| (u32::MAX - i, 40)).collect(), id: u16::MAX };
    let small = Assignment { role: NodeRole::Wifi5, plan: heapless::Vec::from_slice(&[(149, 150), (6, 40)]).unwrap(), id: 77 };
    let mut b1 = [0u8; JOB_MAX];
    let mut b2 = [0u8; JOB_MAX];
    let s1 = encode_job(&big, 0xFE, &mut b1).unwrap().to_vec();
    let s2 = encode_job(&small, 3, &mut b2).unwrap().to_vec();
    let seeds: [&[u8]; 2] = [&s1, &s2];
    fuzzkit::run("nodeimage::decode_job", &seeds, ITERS, |bytes| {
        if let Some(StoredJob { job, streak }) = decode_job(bytes) {
            let mut buf = [0u8; JOB_MAX];
            let again = encode_job(&job, streak, &mut buf).expect("a decoded job re-encodes");
            assert_eq!(decode_job(again), Some(StoredJob { job, streak }));
        }
    });
}

fn entry(ty: u8, subtype: u8, offset: u32, size: u32, label: &str) -> [u8; keystore::ENTRY_LEN] {
    let mut e = [0u8; keystore::ENTRY_LEN];
    e[..2].copy_from_slice(&[0xAA, 0x50]);
    e[2] = ty;
    e[3] = subtype;
    e[4..8].copy_from_slice(&offset.to_le_bytes());
    e[8..12].copy_from_slice(&size.to_le_bytes());
    e[12..12 + label.len()].copy_from_slice(label.as_bytes());
    e
}

#[test]
fn fuzz_keystore_search() {
    let mut table = Vec::new();
    for e in [entry(1, 2, 0x9000, 0x5000, "nvs"), entry(0, 0x10, 0x10000, 0x180000, "ota_0"), entry(1, 6, 0x3F0000, 0x1000, "rskey")] {
        table.extend_from_slice(&e);
    }
    table.extend_from_slice(&[0xFF; 32]);
    let seeds: [&[u8]; 1] = [&table];
    fuzzkit::run("nodeimage::keystore::find", &seeds, ITERS, |bytes| {
        let mut reads = 0u32;
        let r = keystore::find(|at, out| {
            reads += 1;
            let i = (at - keystore::TABLE_OFFSET) as usize;
            match bytes.get(i..i + keystore::ENTRY_LEN) {
                Some(b) => {
                    out.copy_from_slice(b);
                    true
                }
                None => false,
            }
        });
        assert!(reads <= keystore::MAX_ENTRIES);
        if let Found::At { .. } = r {
            // Only a whole entry labelled rskey, of the right kind, is taken.
            let hit = bytes.chunks_exact(keystore::ENTRY_LEN).filter_map(|c| keystore::parse_entry(c.try_into().unwrap())).find(|e| e.label() == "rskey");
            let e = hit.expect("found without an rskey entry");
            assert!(e.size >= keystore::PARTITION_LEN);
        }
    });
}

const KA: [u8; 32] = [0x11; 32];
const MAC_A: [u8; 6] = [0x02, 0, 0, 0, 0, 0x0a];
const GW: [u8; 6] = [0x02, 0, 0, 0, 0, 0x99];

fn fixed_rng() -> u32 {
    0x7000
}

fn keyed_link() -> uplink::Link {
    use kitkeys::{KeyCheck, KitId, NodeIdent, NodeKey, record::NodeRecord};
    let key = NodeKey::from_bytes(KA);
    let rec = NodeRecord { ident: NodeIdent { slot: 0, generation: 1, mac: MAC_A }, kit_id: KitId([1, 2, 3, 4]), key_check: KeyCheck([0; 4]), key };
    uplink::Link::new(kitkeys::record::State::Keyed(rec), MAC_A, Some(uplink::RngSource { next: fixed_rng, name: "test" }))
}

/// The node hears the hub's beacon and sends one batch, as on the air.
/// Deterministic (fixed random source), so every fresh link ends up in the
/// same session. Returns the batch.
fn prime(link: &mut uplink::Link, beacon: &[u8]) -> Vec<u8> {
    assert!(matches!(link.receive(&GW, beacon, 1_000), uplink::Rx::Gateway));
    let mut batch = airlink::Batch::new();
    assert!(batch.push(&[2, 7, 0]));
    link.packet(&mut batch, 1, 1_000).expect("may send after a beacon").to_vec()
}

/// A beacon, a sealed command, an open command holding a TimeSync, an open
/// beacon and a bare command frame.
fn rx_seeds() -> Vec<Vec<u8>> {
    use airlink::v3::Hub;
    use kitkeys::NodeKey;
    let mut h = Hub::new(3);
    h.set_gateway(GW);
    assert!(h.set_slot(0, MAC_A, &NodeKey::from_bytes(KA)));
    let mut b = [0u8; airlink::v3::BEACON_MAX];
    let n = h.seal_beacon(&mut b, 1, 6, 50_000).unwrap();
    let beacon = b[..n].to_vec();
    // The hub must hear the node before it can command it.
    let mut node = keyed_link();
    let up = prime(&mut node, &beacon);
    assert!(matches!(h.open_frames(&MAC_A, &up, 50_000), airlink::v3::HubVerdict::Accept(_)));
    let mut cmd = [0u8; 64];
    let ts = link::encode_command(&Command::TimeSync { utc_ms: 1_700_000_000_000, pps: true }, &mut cmd).unwrap().to_vec();
    let mut c = [0u8; 128];
    // A TimeSync needs a proven link; a plain command does not.
    let mut pc = [0u8; 64];
    let poll = link::encode_command(&Command::JoinSlot { epoch: 3, slots: 4, slot_us: 500 }, &mut pc).unwrap().to_vec();
    let n = h.seal_command(0, &mut c, 2, &poll, 50_000).unwrap();
    let sealed = c[..n].to_vec();
    let mut o = [0u8; 128];
    let open = airlink::command(&mut o, 4, &ts).unwrap().to_vec();
    let mut ob = [0u8; airlink::BEACON_LEN];
    let open_beacon = airlink::beacon(&mut ob, 5, 6).to_vec();
    std::vec![beacon, sealed, open, open_beacon, ts]
}

#[test]
fn fuzz_link_receive_and_commands() {
    let seeds = rx_seeds();
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    fuzzkit::run("nodeimage::uplink::Link::receive", &refs, ITERS, |bytes| {
        let mut n = 0;
        uplink::commands(bytes, |_| n += 1);
        let _ = uplink::has_timesync(bytes);
        assert!(n <= bytes.iter().filter(|&&b| b == 0).count());
        let mut open = uplink::Link::new(kitkeys::record::State::Open, MAC_A, None);
        let mut keyed = keyed_link();
        prime(&mut keyed, &seeds[0]);
        for (k, link) in [&mut open, &mut keyed].into_iter().enumerate() {
            for src in [GW, MAC_A] {
                if let uplink::Rx::Commands(p) = link.receive(&src, bytes, 1_010) {
                    if k == 1 {
                        // Keyed: only the genuine command, from the gateway.
                        assert_eq!(bytes, seeds[1].as_slice(), "a changed command was taken");
                        assert_eq!(src, GW);
                    }
                    let (o, i) = (bytes.as_ptr_range(), p.as_ptr_range());
                    assert!(o.start <= i.start && i.end <= o.end);
                    uplink::commands(p, |_| {});
                }
            }
        }
    });
}

/// The sealed command seed reaches the deep path.
#[test]
fn sealed_command_seed_is_taken() {
    let seeds = rx_seeds();
    let mut keyed = keyed_link();
    prime(&mut keyed, &seeds[0]);
    let mut got = Vec::new();
    if let uplink::Rx::Commands(p) = keyed.receive(&GW, &seeds[1], 1_010) {
        uplink::commands(p, |c| got.push(c));
    }
    assert!(matches!(got.as_slice(), [Command::JoinSlot { .. }]), "{got:?} {:?}", keyed.counts);
}
