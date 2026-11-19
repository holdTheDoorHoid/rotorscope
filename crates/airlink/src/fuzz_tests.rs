//! Fuzz-style tests (crates/fuzzkit) for every packet parser in this crate:
//! the keyless `v3::inspect` and its helpers, the version 1 parser, the hub's
//! and the node's open-and-verify paths with fixed test keys, the gateway
//! relay's classifier and its console-line parsers.
//!
//! The keys are obvious test values ([0x11; 32] and so on), as in v3/tests.rs.

use std::vec::Vec;

use kitkeys::{Mac, NodeKey, SLOTS};

use crate::relay::{self, Outcome};
use crate::v3::{self, Hub, HubVerdict, Kind, Node, NodeKeys, NodeVerdict};
use crate::{Batch, HEADER, MAX_PACKET};

const ITERS: usize = 2_000;

const KA: [u8; 32] = [0x11; 32];
const KB: [u8; 32] = [0x22; 32];
const MAC_A: Mac = [0x02, 0, 0, 0, 0, 0x0a];
const MAC_B: Mac = [0x02, 0, 0, 0, 0, 0x0b];
const GW: Mac = [0x02, 0, 0, 0, 0, 0x99];
const EPOCH: u32 = 7;
/// Hub and node uptimes when the seeds were sealed.
const HT: u64 = 1_000_000;
const NT: u64 = 10_000;

/// Every node made here starts the same session, so a sealed seed stays
/// valid for a fresh node.
fn fixed_rng() -> u32 {
    0x1000
}

fn hub() -> Hub {
    let mut h = Hub::new(EPOCH);
    h.set_gateway(GW);
    assert!(h.set_slot(0, MAC_A, &NodeKey::from_bytes(KA)));
    assert!(h.set_slot(1, MAC_B, &NodeKey::from_bytes(KB)));
    h
}

fn node() -> Node {
    Node::new(NodeKeys::new(0, MAC_A, &NodeKey::from_bytes(KA)), fixed_rng)
}

/// Genuine packets of every kind: beacon, uplink batch, command, sync, an
/// unprovisioned announcement and a version 1 packet.
struct Seeds {
    beacon: Vec<u8>,
    uplink: Vec<u8>,
    command: Vec<u8>,
    sync: Vec<u8>,
    all: Vec<Vec<u8>>,
}

fn seeds() -> Seeds {
    let mut h = hub();
    let mut n = node();
    let mut b = [0u8; v3::BEACON_MAX];
    let len = h.seal_beacon(&mut b, 1, 6, HT).unwrap();
    let beacon = b[..len].to_vec();
    assert!(matches!(n.accept(&GW, &beacon, NT, false), NodeVerdict::Accept(_)));
    let mut batch = Batch::new();
    assert!(batch.push(&[3, 1, 2, 0]));
    assert!(batch.push(&[2, 9, 0]));
    let uplink = batch.sealed_v3(2, &mut n, NT).unwrap().to_vec();
    let v = h.open_frames(&MAC_A, &uplink, HT);
    assert!(matches!(v, HubVerdict::Accept(_)), "{v:?}");
    let mut c = [0u8; 64];
    let len = h.seal_command(0, &mut c, 3, &[4, 1, 2, 3, 0], HT).unwrap();
    let command = c[..len].to_vec();
    let mut s = [0u8; v3::SYNC_LEN];
    let len = h.seal_sync(0, (n.session(), 1), &mut s, 4, 6, HT).unwrap();
    let sync = s[..len].to_vec();
    let mut u = [0u8; v3::UNPROVISIONED_LEN];
    let unprov = v3::unprovisioned(&mut u, 5, &v3::Unprovisioned { mac: MAC_B, image: 1, firmware: *b"node-wifi 0.1.0\0" }).unwrap().to_vec();
    let mut o = [0u8; 64];
    let open = crate::command(&mut o, 6, &[4, 1, 2, 3, 0]).unwrap().to_vec();
    let all = std::vec![beacon.clone(), uplink.clone(), command.clone(), sync.clone(), unprov, open];
    Seeds { beacon, uplink, command, sync, all }
}

fn refs(v: &[Vec<u8>]) -> Vec<&[u8]> {
    v.iter().map(|s| s.as_slice()).collect()
}

fn within(outer: &[u8], inner: &[u8]) -> bool {
    let (o, i) = (outer.as_ptr_range(), inner.as_ptr_range());
    inner.is_empty() || (o.start <= i.start && i.end <= o.end)
}

#[test]
fn fuzz_inspect_and_keyless_parsers() {
    let s = seeds();
    let cfg = fuzzkit::Config::new().max_len(MAX_PACKET + 40);
    fuzzkit::run_with("airlink::v3::inspect", &cfg, &refs(&s.all), ITERS * 4, |bytes| {
        if let Ok(shape) = v3::inspect(bytes) {
            assert_eq!(shape.len, bytes.len());
            assert!(shape.len <= MAX_PACKET);
            let p = v3::payload(bytes, &shape);
            assert!(within(bytes, p));
            assert_eq!(p.len(), shape.payload_len);
            if shape.kind == Kind::Beacon {
                assert!((1..=SLOTS).contains(&usize::from(shape.slots)));
            }
            for f in v3::frames(p) {
                assert!(within(p, f) && f.last() == Some(&0));
            }
        }
        if let Some(u) = v3::parse_unprovisioned(bytes) {
            // Re-encoding gives the same bytes back.
            let mut out = [0u8; v3::UNPROVISIONED_LEN];
            let again = v3::unprovisioned(&mut out, u16::from_le_bytes([bytes[4], bytes[5]]), &u).unwrap();
            assert_eq!(again, bytes);
        }
        if let Some(p) = crate::parse(bytes) {
            assert!(within(bytes, p.payload));
            assert_eq!(p.payload.len(), bytes.len() - HEADER);
            let _ = crate::beacon_channel(&p);
            for f in crate::frames(&p) {
                assert!(within(p.payload, f));
            }
        }
        let _ = relay::fingerprint(bytes);
    });
}

#[test]
fn fuzz_hub_open_frames() {
    let s = seeds();
    fuzzkit::run_with("airlink::v3::Hub::open_frames", &fuzzkit::Config::new().max_len(MAX_PACKET + 8), &refs(&s.all), ITERS, |bytes| {
        for src in [MAC_A, MAC_B, GW] {
            let mut h = hub();
            match h.open_frames(&src, bytes, HT) {
                HubVerdict::Accept(up) => {
                    // Only the genuine batch, from its own sender, opens.
                    assert_eq!(bytes, s.uplink.as_slice(), "a changed batch was accepted");
                    assert_eq!(src, MAC_A);
                    assert_eq!(up.slot, 0);
                    assert!(usize::from(up.slot) < SLOTS);
                    assert!(within(bytes, up.payload));
                }
                HubVerdict::Reject { genuine: Some(_), .. } => {
                    assert_eq!(bytes, s.uplink.as_slice(), "a changed batch had a good tag");
                }
                HubVerdict::Reject { slot: Some(slot), .. } | HubVerdict::Duplicate { slot } => {
                    assert!(usize::from(slot) < SLOTS);
                    assert_eq!(h.slot_of(&src), Some(slot));
                }
                HubVerdict::Reject { slot: None, .. } => {}
            }
        }
    });
}

#[test]
fn fuzz_node_accept() {
    let s = seeds();
    let genuine: Vec<&[u8]> = std::vec![&s.beacon, &s.command, &s.sync];
    fuzzkit::run_with("airlink::v3::Node::accept", &fuzzkit::Config::new().max_len(MAX_PACKET + 8), &refs(&s.all), ITERS, |bytes| {
        for (heard_beacon, timesync) in [(false, false), (true, false), (true, true)] {
            let mut n = node();
            if heard_beacon {
                assert!(matches!(n.accept(&GW, &s.beacon, NT, false), NodeVerdict::Accept(_)));
            }
            for src in [GW, MAC_B] {
                if let NodeVerdict::Accept(d) = n.accept(&src, bytes, NT + 5, timesync) {
                    // A beacon carries one tag per slot and node A checks
                    // only its own (slot 0): the other slots' tags are not
                    // its to judge. Everything else must be genuine.
                    let mine = v3::BEACON_FIXED + v3::BEACON_TAG_LEN;
                    let beacon_ok = d.kind == Kind::Beacon && bytes.len() == s.beacon.len() && bytes[..mine] == s.beacon[..mine];
                    assert!(beacon_ok || genuine.contains(&bytes), "a changed packet was accepted: {d:?}");
                    assert_eq!(src, GW, "accepted from an address that is not the gateway");
                    assert!(within(bytes, d.payload));
                }
            }
        }
    });
}

#[test]
fn fuzz_relay_classify() {
    let s = seeds();
    fuzzkit::run_with("airlink::relay::Uplink::offer", &fuzzkit::Config::new().max_len(MAX_PACKET + 40), &refs(&s.all), ITERS * 2, |bytes| {
        let mut up = relay::Uplink::<16>::new();
        for (i, roster) in [&[][..], &[MAC_A][..]].into_iter().enumerate() {
            if i == 1 {
                up.set_roster(roster);
            }
            let out = up.offer(MAC_A, -50, bytes, 1_000 + i as u64 * 10, 0);
            if out == Outcome::Queued {
                assert!(bytes.len() <= MAX_PACKET);
                assert!(bytes.starts_with(&crate::MAGIC));
            }
        }
        let mut sent = 0;
        while up.pop(5_000, |q| assert!(q.data().len() <= MAX_PACKET)).is_some() {
            sent += 1;
            assert!(sent <= 16);
        }
    });
}

#[test]
fn fuzz_relay_console_lines() {
    let seeds: [&[u8]; 6] = [
        b"gwforge batch",
        b"gwforge flip 02:00:00:00:00:0a",
        b"gwforge delay 250",
        b"gwforge drop 02:00:00:00:00:0b 40 steady",
        b"gwforge drop off",
        b"02:00:00:00:00:0a",
    ];
    let dict: [&[u8]; 6] = [b"gwforge", b" ", b":", b"drop", b"steady", b"ff"];
    let cfg = fuzzkit::Config::new().max_len(80).dict(&dict);
    fuzzkit::run_with("airlink::relay::parse_forge", &cfg, &seeds, ITERS * 4, |bytes| {
        let line = fuzzkit::text(bytes);
        let _ = relay::parse_forge(&line);
        if let Some(m) = relay::parse_mac(&line) {
            let back = std::format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
            assert_eq!(relay::parse_mac(&back), Some(m));
        }
    });
}
