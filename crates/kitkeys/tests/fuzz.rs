//! Fuzz-style tests (crates/fuzzkit) for the flash-record readers: the node
//! record, the hub's master and roster records, the boot counter log, and
//! the kit file text. The master key here is the test key 00 01 .. 1f from
//! src/tests.rs, never a real one.

use kitkeys::roster::{Entry, Roster};
use kitkeys::{KeyCheck, KitId, MasterKey, NodeIdent, SoftKeySource, bootlog, hub, record};

const ITERS: usize = 4_000;

fn m1() -> MasterKey {
    let mut b = [0u8; 32];
    for (i, x) in b.iter_mut().enumerate() {
        *x = i as u8;
    }
    MasterKey::from_bytes(b).unwrap()
}

const MAC: [u8; 6] = [0x24, 0x58, 0x7c, 0xaa, 0xbb, 0xcc];

/// Lay `bytes` into an erased image of `len` bytes: the first 64 at copy A,
/// the next 64 at copy B (offset `b`), anything after that from `b + 64`.
fn image(bytes: &[u8], len: usize, b: usize) -> Vec<u8> {
    let mut p = vec![0xFF; len];
    let (a, rest) = bytes.split_at(bytes.len().min(64));
    p[..a.len()].copy_from_slice(a);
    let (bb, tail) = rest.split_at(rest.len().min(64));
    p[b..b + bb.len()].copy_from_slice(bb);
    let t = tail.len().min(len - b - 64);
    p[b + 64..b + 64 + t].copy_from_slice(&tail[..t]);
    p
}

#[test]
fn fuzz_node_record() {
    let mut src = SoftKeySource::new(m1());
    let ident = NodeIdent { slot: 3, generation: 1, mac: MAC };
    let rec = record::NodeRecord::provision(&mut src, ident).unwrap();
    let keyed = record::encode(&rec).0;
    let open = record::encode_open().0;
    let mut both = keyed.to_vec();
    both.extend_from_slice(&open);
    let seeds: [&[u8]; 3] = [&keyed, &open, &both];
    fuzzkit::run("kitkeys::record::read", &seeds, ITERS, |bytes| {
        let _ = record::read(bytes, &MAC);
        let p = image(bytes, record::PARTITION_LEN, record::COPY_OFFSETS[1]);
        match record::read(&p, &MAC) {
            record::State::Keyed(r) => {
                assert_eq!(r.ident.mac, MAC, "a keyed record for another board was taken");
                // Re-encoding what was read reads back the same.
                let again = record::partition_image(&record::encode(&r));
                match record::read(&again.0, &MAC) {
                    record::State::Keyed(b) => {
                        assert_eq!(b.ident, r.ident);
                        assert_eq!(b.kit_id, r.kit_id);
                        assert_eq!(b.key_check, r.key_check);
                    }
                    other => panic!("re-encoded record read as {other:?}"),
                }
            }
            record::State::WrongBoard { record_mac } => assert_ne!(record_mac, MAC),
            _ => {}
        }
    });
}

fn roster() -> Roster {
    let mut r = Roster::new();
    for (slot, last) in [(0u8, 1u8), (3, 2), (15, 3)] {
        r.insert(Entry { slot, generation: u16::from(slot) + 1, mac: [2, 0, 0, 0, 0, last], key_check: KeyCheck([slot; 4]), revoked: slot == 3 })
            .unwrap();
    }
    r
}

#[test]
fn fuzz_hub_master_record() {
    let m = m1();
    let kit = m.kit_id();
    let keyed = hub::encode_master_keyed(Some(&m), kit);
    let element = hub::encode_master_keyed(None, kit);
    let open = hub::encode_master_open();
    let seeds: [&[u8]; 3] = [&keyed.0, &element.0, &open.0];
    fuzzkit::run("kitkeys::hub::read_master", &seeds, ITERS, |bytes| {
        let _ = hub::read_master(bytes);
        let p = image(bytes, hub::PARTITION_LEN, hub::MASTER_OFFSETS[1]);
        if let hub::MasterState::Good(hub::Master::Keyed { kit_id, master: Some(k) }) = hub::read_master(&p) {
            // The recorded kit id always matches the master it holds.
            assert_eq!(k.kit_id(), kit_id);
        }
    });
}

#[test]
fn fuzz_hub_roster_record() {
    let kit = m1().kit_id();
    let rec = hub::encode_roster(kit, &roster());
    let empty = hub::encode_roster(kit, &Roster::new());
    let seeds: [&[u8]; 2] = [&rec, &empty];
    let cfg = fuzzkit::Config::new().max_len(2 * hub::ROSTER_LEN + 16);
    fuzzkit::run_with("kitkeys::hub::read_roster", &cfg, &seeds, ITERS, |bytes| {
        let _ = hub::read_roster(bytes, kit);
        // Copy A from the input, copy B from what follows it.
        let mut s = vec![0xFF; hub::SECTOR_LEN];
        let (a, b) = bytes.split_at(bytes.len().min(hub::ROSTER_LEN));
        s[..a.len()].copy_from_slice(a);
        let b = &b[..b.len().min(hub::ROSTER_LEN)];
        s[hub::ROSTER_OFFSETS[1]..hub::ROSTER_OFFSETS[1] + b.len()].copy_from_slice(b);
        for k in [kit, KitId([0; 4])] {
            match hub::read_roster(&s, k) {
                hub::RosterState::Good(r) => {
                    // Slots in range, each once, and the record re-encodes
                    // to one that reads back the same.
                    let mut seen = [false; kitkeys::SLOTS];
                    for e in r.entries() {
                        assert!(usize::from(e.slot) < kitkeys::SLOTS);
                        assert!(!seen[usize::from(e.slot)]);
                        seen[usize::from(e.slot)] = true;
                    }
                    let again = hub::encode_roster(k, &r);
                    assert_eq!(hub::read_roster(&hub::roster_sector(&again), k), hub::RosterState::Good(r));
                }
                hub::RosterState::OtherKit(o) => assert_ne!(o, k),
                _ => {}
            }
        }
    });
}

#[test]
fn fuzz_boot_log() {
    let mut seq = Vec::new();
    for v in [5u32, 6, 7] {
        seq.extend_from_slice(&bootlog::entry(v));
    }
    let max = bootlog::entry(bootlog::MAX_VALUE - 1);
    let seeds: [&[u8]; 3] = [&seq, &max, &bootlog::entry(0)];
    fuzzkit::run("kitkeys::bootlog::read", &seeds, ITERS, |bytes| {
        let _ = bootlog::read(bytes);
        // The first two bytes pick where the rest is written: a sector's
        // start, its end, or the other sector.
        let (at, body) = match bytes {
            [a, b, rest @ ..] => (usize::from(u16::from_le_bytes([*a, *b])) % bootlog::PARTITION_LEN, rest),
            _ => (0, bytes),
        };
        let at = at - at % bootlog::ENTRY_LEN;
        let mut p = vec![0xFF; bootlog::PARTITION_LEN];
        let n = body.len().min(bootlog::PARTITION_LEN - at);
        p[at..at + n].copy_from_slice(&body[..n]);
        if let bootlog::State::Good { latest, next } = bootlog::read(&p) {
            assert_eq!(next.value, latest + 1);
            assert!(next.offset % bootlog::ENTRY_LEN == 0 && next.offset + bootlog::ENTRY_LEN <= bootlog::PARTITION_LEN);
            // Recording the boot makes it the latest: the counter only grows.
            bootlog::apply(&mut p, &next);
            match bootlog::read(&p) {
                bootlog::State::Good { latest: l, .. } => assert_eq!(l, next.value),
                bootlog::State::Exhausted { latest: l } => assert_eq!(l, next.value),
                other => panic!("after recording boot {} the log reads {other:?}", next.value),
            }
        }
    });
}

#[test]
fn fuzz_kit_file() {
    let text = m1().to_kit_file();
    let seeds: [&[u8]; 1] = [text.as_bytes()];
    let dict: [&[u8]; 4] = [b" ", b"\n", b"0", b"f"];
    let cfg = fuzzkit::Config::new().max_len(200).dict(&dict);
    fuzzkit::run_with("kitkeys::MasterKey::from_kit_file", &cfg, &seeds, ITERS, |bytes| {
        if let Ok(m) = MasterKey::from_kit_file(bytes) {
            let again = m.to_kit_file();
            let back = MasterKey::from_kit_file(again.as_bytes()).expect("a written kit file reads back");
            assert_eq!(back.kit_id(), m.kit_id());
        }
    });
}
