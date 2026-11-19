//! Host tests. The known-answer vectors were computed independently with
//! Python's `hmac`, `hashlib` and `zlib` (a 50-line script, not in the
//! repository), from the definitions in docs/LINK-KEYS-DESIGN.md 4.1 and
//! the record layouts in `record.rs`, `hub.rs` and `bootlog.rs`, so the Rust
//! is not only tested against itself.

use super::*;
use crate::roster::{Roster, RosterError};

fn unhex<const N: usize>(s: &str) -> [u8; N] {
    assert_eq!(s.len(), 2 * N, "{s}");
    let mut o = [0u8; N];
    for i in 0..N {
        o[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    o
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// M1 = 00 01 02 .. 1f.
fn m1() -> MasterKey {
    let mut b = [0u8; 32];
    for (i, x) in b.iter_mut().enumerate() {
        *x = i as u8;
    }
    MasterKey::from_bytes(b).unwrap()
}

/// M2 = SHA-256("rotorscope test master 2").
fn m2() -> MasterKey {
    MasterKey::from_bytes(unhex("a4ff0d4811e41324e51da0acb3c5a9df5a75db3714f01567c851800531edc459")).unwrap()
}

const A: NodeIdent = NodeIdent { slot: 3, generation: 1, mac: [0x24, 0x58, 0x7c, 0xaa, 0xbb, 0xcc] };
const B: NodeIdent = NodeIdent { slot: 15, generation: 0x0102, mac: [0x02, 0, 0, 0, 0, 0x01] };

struct Vector {
    node: &'static str,
    up: &'static str,
    down: &'static str,
    check: &'static str,
}

const M1_A: Vector = Vector {
    node: "0d897e0e2ff21b323d2640e53d97f0a2b59d3ff988afb723993e930689894a22",
    up: "0922d68e1b3042172ffe933990dde8a9ee23cfba4ef17ad679c9d043c52ae0a0",
    down: "7c73c58ae7e0115dd01f0906fe83498b982f276a514181987c9c85331dc4fe05",
    check: "096f7f75",
};
const M1_B: Vector = Vector {
    node: "a3ad1c99096ee8de2628e9ec955bdcce1a0515e018fadab5fda1b587de1056c4",
    up: "ba25bab052bab1d1bc942c44bfa19d6888c4e648aa8d8624efb1b3baa2299728",
    down: "4c4040a8ae0c29e5bdf724b237b1d3c37ce61677764fcd903dd4546243b9ee7f",
    check: "ee4200f1",
};
const M2_A: Vector = Vector {
    node: "a7c86fc3dff96e3eff355e37b9f8e34b52dd0958cba15bdf7c92007fde5be6e4",
    up: "29208a1774c7a299b47797fcd90fefe5df30a0d4d85ca186231b890fc55c2fd0",
    down: "5a631c7bdfd352ec6fd11165338b63078bddba948be18ce1b343255e71b63a69",
    check: "e0f4fdb9",
};
const M2_B: Vector = Vector {
    node: "8ebec42ada5460fd9b9e867df0989acdbd9e815aad1d5f45042e5b983ad2bddc",
    up: "484871d5a9cf3bdabf3cca830a43e2c306d0d3377b906c885268515b02f482e2",
    down: "dcfba021bfbc6b681df3995497b4f4431dd92ff3283d752087e498bc5068dcb5",
    check: "cec9cd44",
};

fn check_vector(src: &mut SoftKeySource, ident: &NodeIdent, v: &Vector) {
    let k = src.derive(ident).unwrap();
    assert_eq!(hex(k.bytes()), v.node, "K_node {ident:?}");
    // K_up / K_down: a keyed state; HMAC under the expected raw key must match.
    let probe: &[u8] = b"probe";
    let up = Hmac::new(&unhex::<32>(v.up)).mac(&[probe]);
    let down = Hmac::new(&unhex::<32>(v.down)).mac(&[probe]);
    assert_eq!(k.up().mac(&[probe]), up, "K_up {ident:?}");
    assert_eq!(k.down().mac(&[probe]), down, "K_down {ident:?}");
    assert_eq!(k.check().to_string(), v.check, "key_check {ident:?}");
}

// ------------------------------------------------------------ derivation --

#[test]
fn hmac_matches_rfc4231_case_2() {
    // Guards the borrowed primitive itself.
    let m = Hmac::new(b"Jefe").mac(&[b"what do ya want ", b"for nothing?"]);
    assert_eq!(hex(&m), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
}

#[test]
fn kit_id_known_answers() {
    assert_eq!(m1().kit_id().to_string(), "471f9577");
    assert_eq!(m2().kit_id().to_string(), "8937fc95");
    let mut s = SoftKeySource::new(m1());
    assert_eq!(s.kit_id().unwrap().to_string(), "471f9577");
}

#[test]
fn node_key_known_answers() {
    let mut s1 = SoftKeySource::new(m1());
    check_vector(&mut s1, &A, &M1_A);
    check_vector(&mut s1, &B, &M1_B);
    let mut s2 = SoftKeySource::new(m2());
    check_vector(&mut s2, &A, &M2_A);
    check_vector(&mut s2, &B, &M2_B);
}

#[test]
fn node_key_input_layout() {
    // generation is little-endian: 0x0102 -> 02 01.
    let m = node_key_input(&B);
    assert_eq!(&m[..22], b"rotorscope/node-key/v1");
    assert_eq!(&m[22..], &[15, 0x02, 0x01, 0x02, 0, 0, 0, 0, 0x01]);
}

#[test]
fn node_key_bound_to_mac_slot_and_generation() {
    let mut s = SoftKeySource::new(m1());
    let base = s.derive(&A).unwrap();
    let mut other_mac = A;
    other_mac.mac[5] ^= 1;
    let mut other_slot = A;
    other_slot.slot = 4;
    let mut other_gen = A;
    other_gen.generation = 2;
    for id in [other_mac, other_slot, other_gen] {
        let k = s.derive(&id).unwrap();
        assert!(!k.ct_eq(&base), "{id:?}");
        // A tag under one node's up key does not verify under the other's.
        let msg: &[u8] = b"frames";
        assert_ne!(k.up().mac(&[msg]), base.up().mac(&[msg]), "{id:?}");
        assert_ne!(k.check(), base.check(), "{id:?}");
    }
    // Another master, same identity: different key.
    assert!(!SoftKeySource::new(m2()).derive(&A).unwrap().ct_eq(&base));
}

#[test]
fn up_and_down_keys_differ() {
    let k = SoftKeySource::new(m1()).derive(&A).unwrap();
    let msg: &[u8] = b"same bytes";
    assert_ne!(k.up().mac(&[msg]), k.down().mac(&[msg]));
    // Neither is the node key itself used directly.
    assert_ne!(k.up().mac(&[msg]), Hmac::new(k.bytes()).mac(&[msg]));
}

#[test]
fn derive_checked_flags_a_foreign_roster() {
    let mut s1 = SoftKeySource::new(m1());
    let good = derive_checked(&mut s1, &A, KeyCheck(unhex(M1_A.check))).unwrap();
    assert!(good.is_ok());
    let bad = derive_checked(&mut s1, &A, KeyCheck(unhex(M2_A.check))).unwrap();
    assert_eq!(bad.unwrap_err().got.to_string(), M1_A.check);
}

#[test]
fn software_source_has_no_random() {
    let mut s = SoftKeySource::new(m1());
    let mut buf = [0u8; 4];
    assert_eq!(s.random(&mut buf), Ok(false));
}

// --------------------------------------------------------------- secrets --

#[test]
fn secrets_do_not_print() {
    let m = m1();
    let k = SoftKeySource::new(m1()).derive(&A).unwrap();
    for s in [
        format!("{m:?}"),
        format!("{k:?}"),
        format!("{:?}", k.up()),
        format!("{:?}", SoftKeySource::new(m2())),
    ] {
        assert!(!s.contains("0d897e") && !s.contains("000102") && !s.contains("a4ff0d"), "{s}");
        assert!(s.contains(".."), "{s}");
    }
    let rec = record::NodeRecord::provision(&mut SoftKeySource::new(m1()), A).unwrap();
    let s = format!("{rec:?}");
    assert!(!s.contains("0d897e") && !s.contains("13, 137"), "{s}");
}

#[test]
fn master_zero_is_refused() {
    assert!(MasterKey::from_bytes([0; 32]).is_none());
}

#[test]
fn kit_file_round_trip() {
    let t = m1().to_kit_file();
    let text = std::str::from_utf8(t.as_bytes()).unwrap();
    assert!(text.starts_with("rotorscope kit master v1"));
    assert!(text.ends_with("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\n"));
    let back = MasterKey::from_kit_file(t.as_bytes()).unwrap();
    assert_eq!(back.kit_id(), m1().kit_id());
    // Upper case and extra whitespace are fine.
    let mut up = KIT_FILE_HEADER.to_vec();
    up.extend_from_slice(b"  000102030405060708090A0B0C0D0E0F\n101112131415161718191a1b1c1d1e1f  \n");
    assert_eq!(MasterKey::from_kit_file(&up).unwrap().kit_id(), m1().kit_id());
}

#[test]
fn kit_file_refusals() {
    assert_eq!(MasterKey::from_kit_file(b"00").unwrap_err(), KitFileError::NotAKitFile);
    let mut short = KIT_FILE_HEADER.to_vec();
    short.extend_from_slice(&[b'a'; 62]);
    assert_eq!(MasterKey::from_kit_file(&short).unwrap_err(), KitFileError::BadKey);
    let mut long = KIT_FILE_HEADER.to_vec();
    long.extend_from_slice(&[b'a'; 66]);
    assert_eq!(MasterKey::from_kit_file(&long).unwrap_err(), KitFileError::BadKey);
    let mut odd = KIT_FILE_HEADER.to_vec();
    odd.extend_from_slice(&[b'a'; 65]);
    assert_eq!(MasterKey::from_kit_file(&odd).unwrap_err(), KitFileError::BadKey);
    let mut junk = KIT_FILE_HEADER.to_vec();
    junk.extend_from_slice(&[b'g'; 64]);
    assert_eq!(MasterKey::from_kit_file(&junk).unwrap_err(), KitFileError::BadKey);
    let mut zero = KIT_FILE_HEADER.to_vec();
    zero.extend_from_slice(&[b'0'; 64]);
    assert_eq!(MasterKey::from_kit_file(&zero).unwrap_err(), KitFileError::ZeroKey);
}

#[test]
fn crc_matches_zlib() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
}

// ---------------------------------------------------------- node records --

const NODE_RECORD_M1_A: &str = "52534b310103010024587caabbcc471f95770d897e0e2ff21b323d2640e53d97f0a2b59d3ff988afb723993e930689894a22096f7f75000000000000f6669ded";
const OPEN_RECORD: &str = "52534b3102000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002c0a301e";

fn rec_a() -> record::RecordBytes {
    record::encode(&record::NodeRecord::provision(&mut SoftKeySource::new(m1()), A).unwrap())
}

#[test]
fn node_record_known_answer() {
    assert_eq!(hex(&rec_a().0), NODE_RECORD_M1_A);
    assert_eq!(hex(&record::encode_open().0), OPEN_RECORD);
}

#[test]
fn node_record_round_trip() {
    let img = record::partition_image(&rec_a());
    assert_eq!(img.0.len(), 0x1000);
    assert!(img.0[64..0x800].iter().all(|&b| b == 0xFF));
    match record::read(&img.0, &A.mac) {
        record::State::Keyed(r) => {
            assert_eq!(r.ident, A);
            assert_eq!(r.kit_id.to_string(), "471f9577");
            assert_eq!(hex(r.key.bytes()), M1_A.node);
            assert_eq!(r.auth_summary().to_string(), "kit:471f9577 slot:3 gen:1 check:096f7f75");
        }
        s => panic!("{s:?}"),
    }
}

#[test]
fn open_record_reads_open() {
    let img = record::partition_image(&record::encode_open());
    let s = record::read(&img.0, &A.mac);
    assert_eq!(s.name(), "open");
}

#[test]
fn erased_partition_is_missing() {
    assert_eq!(record::read(&[0xFF; 0x1000], &A.mac).name(), "missing");
    assert_eq!(record::read(&[0x00; 0x1000], &A.mac).name(), "missing");
}

#[test]
fn one_damaged_copy_uses_the_other() {
    for (o, bit) in [(0usize, 0usize), (0, 59), (0, 63), (0x800, 20)] {
        let mut img = record::partition_image(&rec_a());
        img.0[o + bit] ^= 0x10;
        let s = record::read(&img.0, &A.mac);
        assert_eq!(s.name(), "keyed", "flip at {o:#x}+{bit}");
    }
    // Copy A erased (half-written partition), copy B good.
    let mut img = record::partition_image(&rec_a());
    img.0[..64].fill(0xFF);
    assert_eq!(record::read(&img.0, &A.mac).name(), "keyed");
}

#[test]
fn both_copies_damaged() {
    let mut img = record::partition_image(&rec_a());
    img.0[10] ^= 1;
    img.0[0x800 + 40] ^= 0x80;
    assert_eq!(record::read(&img.0, &A.mac).name(), "damaged");
    // One damaged, the other erased: still damaged, not missing.
    let mut img = record::partition_image(&rec_a());
    img.0[10] ^= 1;
    img.0[0x800..0x840].fill(0xFF);
    assert_eq!(record::read(&img.0, &A.mac).name(), "damaged");
}

#[test]
fn good_crc_but_nonsense_is_damaged() {
    // A record whose CRC was recomputed after the key changed: the check
    // value no longer belongs to the key.
    let mut r = rec_a().0;
    r[20] ^= 1;
    let c = crc32(&r[..60]);
    r[60..].copy_from_slice(&c.to_le_bytes());
    assert_eq!(record::read_copies(&r, &r, &A.mac).name(), "damaged");
    // Unknown type.
    let mut r = rec_a().0;
    r[4] = 7;
    let c = crc32(&r[..60]);
    r[60..].copy_from_slice(&c.to_le_bytes());
    assert_eq!(record::read_copies(&r, &r, &A.mac).name(), "damaged");
}

#[test]
fn first_good_copy_wins() {
    let b = record::encode(&record::NodeRecord::provision(&mut SoftKeySource::new(m2()), A).unwrap());
    let a = rec_a();
    match record::read_copies(&a.0, &b.0, &A.mac) {
        record::State::Keyed(r) => assert_eq!(r.kit_id, m1().kit_id()),
        s => panic!("{s:?}"),
    }
    match record::read_copies(&b.0, &a.0, &A.mac) {
        record::State::Keyed(r) => assert_eq!(r.kit_id, m2().kit_id()),
        s => panic!("{s:?}"),
    }
}

#[test]
fn record_on_another_board_is_wrong_board() {
    let img = record::partition_image(&rec_a());
    let other = [0x24, 0x58, 0x7c, 0xaa, 0xbb, 0xcd];
    match record::read(&img.0, &other) {
        record::State::WrongBoard { record_mac } => assert_eq!(record_mac, A.mac),
        s => panic!("{s:?}"),
    }
}

// ---------------------------------------------------------------- roster --

#[test]
fn roster_takes_free_slots_then_bumps_generation() {
    let mut s = SoftKeySource::new(m1());
    let mut r = Roster::new();
    let mac = |n: u8| [2, 0, 0, 0, 0, n];
    let a = r.provision(&mut s, None, mac(1)).unwrap().unwrap();
    let b = r.provision(&mut s, None, mac(2)).unwrap().unwrap();
    assert_eq!((a.slot, a.generation, b.slot, b.generation), (0, 1, 1, 1));
    // Re-key slot 0 for a replacement board: generation 2, new MAC.
    let c = r.provision(&mut s, Some(0), mac(9)).unwrap().unwrap();
    assert_eq!((c.slot, c.generation, c.mac), (0, 2, mac(9)));
    let e = r.get(0).unwrap();
    assert_eq!(e.key_check, s.derive(&c).unwrap().check());
    // The old board's key is unrelated to the new one.
    assert!(!s.derive(&a).unwrap().ct_eq(&s.derive(&c).unwrap()));
    // Explicit free slot starts at generation 1.
    let d = r.provision(&mut s, Some(7), mac(7)).unwrap().unwrap();
    assert_eq!((d.slot, d.generation), (7, 1));
    assert_eq!(r.next_free_slot(), Some(2));
    assert_eq!(r.len(), 3);
}

#[test]
fn roster_refusals() {
    let mut s = SoftKeySource::new(m1());
    let mut r = Roster::new();
    let mac = [2, 0, 0, 0, 0, 1];
    r.provision(&mut s, None, mac).unwrap().unwrap();
    assert_eq!(r.plan(None, mac), Err(RosterError::MacInUse { slot: 0 }));
    assert_eq!(r.plan(Some(5), mac), Err(RosterError::MacInUse { slot: 0 }));
    assert!(r.plan(Some(0), mac).is_ok());
    assert_eq!(r.plan(Some(16), [9; 6]), Err(RosterError::NoSuchSlot(16)));
    assert_eq!(r.revoke(3), Err(RosterError::Empty(3)));
    for i in 1..16u8 {
        r.provision(&mut s, None, [3, 0, 0, 0, 0, i]).unwrap().unwrap();
    }
    assert_eq!(r.plan(None, [4; 6]), Err(RosterError::Full));
}

#[test]
fn revoke_and_reprovision() {
    let mut s = SoftKeySource::new(m1());
    let mut r = Roster::new();
    let mac = [2, 0, 0, 0, 0, 1];
    r.provision(&mut s, None, mac).unwrap().unwrap();
    r.revoke(0).unwrap();
    assert!(r.get(0).unwrap().revoked);
    assert_eq!(r.get(0).unwrap().generation, 1);
    // A revoked slot is not free: a new node goes elsewhere.
    assert_eq!(r.next_free_slot(), Some(1));
    // The revoked MAC is no longer live: it may join another slot.
    assert!(r.plan(None, mac).is_ok());
    // Re-provisioning the slot bumps the generation and un-revokes it.
    let n = r.provision(&mut s, Some(0), [2, 0, 0, 0, 0, 2]).unwrap().unwrap();
    assert_eq!(n.generation, 2);
    assert!(!r.get(0).unwrap().revoked);
}

#[test]
fn generation_cannot_wrap() {
    let mut r = Roster::new();
    r.insert(roster::Entry { slot: 2, generation: u16::MAX, mac: [1; 6], key_check: KeyCheck::default(), revoked: false })
        .unwrap();
    assert_eq!(r.plan(Some(2), [1; 6]), Err(RosterError::GenerationExhausted(2)));
}

// ------------------------------------------------------------ hub records --

const HUB_MASTER_M1: &str = "5253483101471f9577000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f00000000000000000000000000000000000000a131c5b9";

fn roster_ab() -> Roster {
    let mut s = SoftKeySource::new(m1());
    let mut r = Roster::new();
    let a = s.derive(&A).unwrap().check();
    let b = s.derive(&B).unwrap().check();
    r.insert(roster::Entry { slot: A.slot, generation: A.generation, mac: A.mac, key_check: a, revoked: false }).unwrap();
    r.insert(roster::Entry { slot: B.slot, generation: B.generation, mac: B.mac, key_check: b, revoked: true }).unwrap();
    r
}

#[test]
fn hub_records_known_answers() {
    assert_eq!(hex(&hub::encode_master_keyed(Some(&m1()), KitId::default()).0), HUB_MASTER_M1);
    let enc = hub::encode_roster(m1().kit_id(), &roster_ab());
    let h = hex(&enc);
    assert!(h.starts_with(
        "52535231471f95770200000003010024587caabbcc096f7f75000f0201020000000001ee4200f101"
    ));
    assert!(h.ends_with("16d98017"), "{h}");
    assert_eq!(enc.len(), 240);
}

#[test]
fn hub_round_trip() {
    let master = hub::encode_master_keyed(Some(&m1()), KitId::default());
    let roster = hub::encode_roster(m1().kit_id(), &roster_ab());
    let img = hub::kit_image(&master, &roster);
    match hub::read_master(&img.0) {
        hub::MasterState::Good(hub::Master::Keyed { kit_id, master: Some(m) }) => {
            assert_eq!(kit_id, m1().kit_id());
            let mut s = SoftKeySource::new(m);
            assert_eq!(hex(s.derive(&A).unwrap().bytes()), M1_A.node);
        }
        s => panic!("{s:?}"),
    }
    assert_eq!(hub::read_roster(&img.0[0x1000..], m1().kit_id()), hub::RosterState::Good(roster_ab()));
    // The roster sector alone (hub-roster) is what the image holds there.
    assert_eq!(hub::roster_sector(&roster)[..], img.0[0x1000..]);
}

#[test]
fn hub_master_with_element() {
    let kid = KitId([1, 2, 3, 4]);
    let rec = hub::encode_master_keyed(None, kid);
    let img = hub::kit_image(&rec, &hub::encode_roster(kid, &Roster::new()));
    match hub::read_master(&img.0) {
        hub::MasterState::Good(hub::Master::Keyed { kit_id, master: None }) => assert_eq!(kit_id, kid),
        s => panic!("{s:?}"),
    }
    assert_eq!(hub::read_roster(&img.0[0x1000..], kid), hub::RosterState::Good(Roster::new()));
}

#[test]
fn hub_open_and_missing_and_damaged() {
    let img = hub::kit_image(&hub::encode_master_open(), &hub::encode_roster(KitId::default(), &Roster::new()));
    assert_eq!(hub::read_master(&img.0).name(), "open");
    assert_eq!(hub::read_master(&[0xFF; 0x2000]).name(), "missing");
    assert_eq!(hub::read_roster(&[0xFF; 0x1000], m1().kit_id()), hub::RosterState::Missing);

    let master = hub::encode_master_keyed(Some(&m1()), KitId::default());
    let roster = hub::encode_roster(m1().kit_id(), &roster_ab());
    // One master copy damaged: the other is used.
    let mut img = hub::kit_image(&master, &roster);
    img.0[12] ^= 1;
    assert_eq!(hub::read_master(&img.0).name(), "keyed");
    img.0[0x800 + 30] ^= 1;
    assert_eq!(hub::read_master(&img.0).name(), "damaged");
    // Roster: one copy damaged, then both.
    let mut img = hub::kit_image(&master, &roster);
    img.0[0x1000 + 20] ^= 1;
    assert!(matches!(hub::read_roster(&img.0[0x1000..], m1().kit_id()), hub::RosterState::Good(_)));
    img.0[0x1800 + 100] ^= 1;
    assert_eq!(hub::read_roster(&img.0[0x1000..], m1().kit_id()), hub::RosterState::Damaged);
}

#[test]
fn hub_master_whose_kit_id_disagrees_is_damaged() {
    let mut r = hub::encode_master_keyed(Some(&m1()), KitId::default()).0;
    r[5] ^= 1;
    let c = crc32(&r[..60]);
    r[60..].copy_from_slice(&c.to_le_bytes());
    let mut img = [0xFFu8; 0x2000];
    img[..64].copy_from_slice(&r);
    assert_eq!(hub::read_master(&img).name(), "damaged");
}

#[test]
fn roster_from_another_kit_is_not_used() {
    let roster = hub::encode_roster(m2().kit_id(), &roster_ab());
    assert_eq!(hub::read_roster(&hub::roster_sector(&roster), m1().kit_id()), hub::RosterState::OtherKit(m2().kit_id()));
}

// --------------------------------------------------------------- bootlog --

#[test]
fn boot_entry_known_answer() {
    assert_eq!(hex(&bootlog::entry(0x1234_5678)), "7856341267bae430");
}

#[test]
fn boot_log_counts_and_wraps_sectors() {
    let mut p = bootlog::seed_image(1000);
    assert_eq!(bootlog::read(&[0xFF; 0x2000]), bootlog::State::Missing);
    let mut last = 1000;
    // Fill sector 0, roll into sector 1, fill it, roll back into sector 0.
    for _ in 0..(bootlog::ENTRIES_PER_SECTOR * 2 + 5) {
        let bootlog::State::Good { latest, next } = bootlog::read(&p) else { panic!() };
        assert_eq!(latest, last);
        assert_eq!(next.value, last + 1);
        bootlog::apply(&mut p, &next);
        last += 1;
    }
    let bootlog::State::Good { latest, next } = bootlog::read(&p) else { panic!() };
    assert_eq!(latest, last);
    assert_eq!(next.offset, 6 * 8);
    assert_eq!(next.erase_sector, None);
}

#[test]
fn boot_log_rolls_over_with_erase() {
    let mut p = bootlog::seed_image(1);
    for _ in 1..bootlog::ENTRIES_PER_SECTOR {
        let bootlog::State::Good { next, .. } = bootlog::read(&p) else { panic!() };
        bootlog::apply(&mut p, &next);
    }
    let bootlog::State::Good { latest, next } = bootlog::read(&p) else { panic!() };
    assert_eq!(latest, 512);
    assert_eq!(next, bootlog::NextBoot { value: 513, offset: 0x1000, erase_sector: Some(1) });
}

#[test]
fn boot_log_skips_torn_entries() {
    let mut p = bootlog::seed_image(50);
    // A torn write after the seed: some bits cleared, check wrong.
    p[8..12].copy_from_slice(&[0x33, 0, 0xFF, 0xFF]);
    let bootlog::State::Good { latest, next } = bootlog::read(&p) else { panic!() };
    assert_eq!(latest, 50);
    // The torn place is not reused.
    assert_eq!(next.offset, 16);
    // Nothing but garbage: damaged.
    let mut g = [0xFFu8; 0x2000];
    g[100] = 0;
    assert_eq!(bootlog::read(&g), bootlog::State::Damaged);
}

#[test]
fn boot_log_never_goes_back_after_interrupted_erase() {
    // Sector 0 full up to 512; erase of sector 1 interrupted (garbage left).
    let mut p = bootlog::seed_image(1);
    for _ in 1..bootlog::ENTRIES_PER_SECTOR {
        let bootlog::State::Good { next, .. } = bootlog::read(&p) else { panic!() };
        bootlog::apply(&mut p, &next);
    }
    p[0x1000..0x1010].fill(0x00);
    let bootlog::State::Good { latest, next } = bootlog::read(&p) else { panic!() };
    assert_eq!((latest, next.value, next.erase_sector), (512, 513, Some(1)));
}

#[test]
fn boot_log_exhausted_and_seed_range() {
    let p = bootlog::seed_image(bootlog::MAX_VALUE);
    assert_eq!(bootlog::read(&p), bootlog::State::Exhausted { latest: bootlog::MAX_VALUE });
    for r in [[0, 0, 0, 0], [0xFF; 4], [0xFE, 0xFF, 0xFF, 0x7F], [1, 2, 3, 4]] {
        let s = bootlog::seed_from_random(r);
        assert!((1..bootlog::SEED_LIMIT).contains(&s), "{s}");
    }
}
