//! Round trips with a key made up here: the test plays the aircraft (SM2
//! wrap to our public key, AES-CTR the payloads, as SPEC.md gives the
//! sender side), then the receiver code above undoes it. No real key, no
//! real record.

extern crate std;
use std::vec::Vec;

use super::*;
use droneid::payload::{self, BLOCK, Record};
use rand_core::{OsRng, RngCore};
use sm2::{NonZeroScalar, Scalar};

pub(crate) const K: [u8; 16] = *b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff";

fn test_key() -> PrivateKey {
    let mut b = [0u8; 32];
    loop {
        OsRng.fill_bytes(&mut b);
        if let Some(k) = PrivateKey::from_bytes(&b) {
            return k;
        }
    }
}

fn deg_to_rad_e7(deg: f64) -> i32 {
    (deg.to_radians() * 1e7).round() as i32
}

/// SPEC.md sender side: C1 = kG, S = kQ, t = SM3(x2||y2||1)[..16],
/// C2 = K ^ t, C3 = SM3(x2||K||y2).
fn wrap(q: &AffinePoint, k: &[u8; 16]) -> ([u8; 64], [u8; 32], [u8; 16]) {
    wrap_with(q, k, random_scalar())
}

/// A random non-zero scalar from the operating system's generator. Bytes are
/// drawn until they are a valid scalar (below n and not zero), which for
/// SM2 is the first draw except about once in 2^32.
fn random_scalar() -> NonZeroScalar {
    loop {
        let mut b = [0u8; 32];
        OsRng.fill_bytes(&mut b);
        if let Ok(s) = NonZeroScalar::try_from(&b[..]) {
            return s;
        }
    }
}

/// [`wrap`] with a given ephemeral scalar (repeatable seeds for the fuzz tests).
pub(crate) fn wrap_with(q: &AffinePoint, k: &[u8; 16], e: NonZeroScalar) -> ([u8; 64], [u8; 32], [u8; 16]) {
    let c1 = (ProjectivePoint::GENERATOR * *e).to_affine().to_sec1_point(false);
    let s = (ProjectivePoint::from(*q) * *e).to_affine().to_sec1_point(false);
    let (x2, y2) = (s.x().unwrap(), s.y().unwrap());
    let t = Sm3::new().chain_update(x2).chain_update(y2).chain_update([0, 0, 0, 1]).finalize();
    let mut c2 = [0u8; 16];
    for i in 0..16 {
        c2[i] = k[i] ^ t[i];
    }
    let c3: [u8; 32] = Sm3::new().chain_update(x2).chain_update(k).chain_update(y2).finalize().into();
    let mut c1b = [0u8; 64];
    c1b[..32].copy_from_slice(c1.x().unwrap());
    c1b[32..].copy_from_slice(c1.y().unwrap());
    (c1b, c3, c2)
}

fn ctr(k: &[u8; 16], iv: &[u8; 8], b: &mut [u8]) {
    SessionKey(*k).ctr(iv, b);
}

pub(crate) fn cryp_aux(serial: &str, lat: f64, lon: f64) -> [u8; 32] {
    let mut p = [0u8; 32];
    p[..serial.len()].copy_from_slice(serial.as_bytes());
    p[16..20].copy_from_slice(&deg_to_rad_e7(lon).to_le_bytes());
    p[20..24].copy_from_slice(&deg_to_rad_e7(lat).to_le_bytes());
    p[24] = 1;
    p
}

fn cryp_core(q: &AffinePoint, k: &[u8; 16], aux: &[u8; 32]) -> Vec<u8> {
    cryp_core_wrapped(k, aux, wrap(q, k))
}

pub(crate) fn cryp_core_wrapped(k: &[u8; 16], aux: &[u8; 32], (c1, c3, c2): ([u8; 64], [u8; 32], [u8; 16])) -> Vec<u8> {
    let iv = [0, 1, 2, 3, 4, 5, 6, 7];
    let mut c = std::vec![0u8; CRYP_LEN];
    c[0] = 0x13;
    c[1..5].copy_from_slice(b"CRYP");
    c[5..9].copy_from_slice(&SessionKey(*k).key_id());
    c[9..73].copy_from_slice(&c1);
    c[73..105].copy_from_slice(&c3);
    c[105..121].copy_from_slice(&c2);
    c[121..129].copy_from_slice(&iv);
    c[129..131].copy_from_slice(&32u16.to_le_bytes());
    let mut a = *aux;
    ctr(k, &iv, &mut a);
    c[131..].copy_from_slice(&a);
    c
}

/// A plausible INFP-10 plaintext over City Hall, Philadelphia.
pub(crate) fn infp10_plain(serial: &str) -> [u8; 109] {
    infp10_at(serial, 7, 1_794_232_987_250)
}

/// The same, with sequence number `seq` and GPS time `gps_ms`.
fn infp10_at(serial: &str, seq: u16, gps_ms: u64) -> [u8; 109] {
    let mut p = [0u8; 109];
    p[0] = 0x10;
    p[1] = 2;
    p[2..4].copy_from_slice(&seq.to_le_bytes());
    let state = Infp10::STATE_MOTORS_ON | Infp10::STATE_IN_AIR | Infp10::STATE_POSITION_VALID;
    p[4..6].copy_from_slice(&state.to_le_bytes());
    p[6..6 + serial.len()].copy_from_slice(serial.as_bytes());
    p[22..26].copy_from_slice(&deg_to_rad_e7(-75.1652).to_le_bytes());
    p[26..30].copy_from_slice(&deg_to_rad_e7(39.9526).to_le_bytes());
    p[30..32].copy_from_slice(&120i16.to_le_bytes());
    p[32..34].copy_from_slice(&800i16.to_le_bytes());
    p[34..36].copy_from_slice(&250i16.to_le_bytes());
    p[40..42].copy_from_slice(&9000i16.to_le_bytes());
    p[42..50].copy_from_slice(&gps_ms.to_le_bytes());
    p[50..54].copy_from_slice(&deg_to_rad_e7(39.9530).to_le_bytes());
    p[54..58].copy_from_slice(&deg_to_rad_e7(-75.1640).to_le_bytes());
    p[58..62].copy_from_slice(&o4plain::HOME_UNSET.to_le_bytes());
    p[62..66].copy_from_slice(&o4plain::HOME_UNSET.to_le_bytes());
    p[66] = 84;
    p
}

pub(crate) fn infp10_core(k: &[u8; 16], plain: &[u8; 109]) -> Vec<u8> {
    let iv = [9, 9, 9, 9, 1, 2, 3, 4];
    let mut c = std::vec![0u8; INFP10_LEN];
    c[0] = 0x10;
    c[1..5].copy_from_slice(b"INFP");
    c[5..9].copy_from_slice(&SessionKey(*k).key_id());
    c[9..17].copy_from_slice(&iv);
    c[17..19].copy_from_slice(&109u16.to_le_bytes());
    let mut b = *plain;
    ctr(k, &iv, &mut b);
    c[19..].copy_from_slice(&b);
    c
}

pub(crate) fn infp11_core(k: &[u8; 16], serial: &str, purpose: &str) -> Vec<u8> {
    let mut p = [0u8; 141];
    p[0] = 0x11;
    p[1..1 + serial.len()].copy_from_slice(serial.as_bytes());
    p[17..20].copy_from_slice(&[0xcc, 0xaa, 0x03]);
    p[20] = purpose.len() as u8;
    p[21..21 + purpose.len()].copy_from_slice(purpose.as_bytes());
    let iv = [7; 8];
    let mut c = std::vec![0u8; INFP11_LEN];
    c[0] = 0x11;
    c[1..5].copy_from_slice(b"INFP");
    c[5..9].copy_from_slice(&SessionKey(*k).key_id());
    c[9..17].copy_from_slice(&iv);
    c[17..19].copy_from_slice(&141u16.to_le_bytes());
    ctr(k, &iv, &mut p);
    c[19..].copy_from_slice(&p);
    c
}

/// The core in a transport block exactly as the spec lays it out, so the
/// receiver's own structural checks (`droneid::payload::parse`) run on it,
/// and the logical object the node would send up.
fn block_and_object(core: &[u8]) -> ([u8; BLOCK], Vec<u8>) {
    let n = core.len();
    let l = n + payload::O4_PADDING;
    let mut b = [0u8; BLOCK];
    b[0] = l as u8;
    b[1..1 + n].copy_from_slice(core);
    let c = droneid::crc::dji_crc16(&b[..l + 1]);
    b[l + 1..l + 3].copy_from_slice(&c.to_le_bytes());
    let obj = b[..l + 3].to_vec();
    (b, obj)
}

pub(crate) fn checked_object(core: &[u8]) -> Vec<u8> {
    let (b, obj) = block_and_object(core);
    let p = payload::parse(&b);
    match p.record {
        Record::Encrypted(e) => assert!(e.valid(), "{:?}", e.checks),
        other => panic!("not an O4 record: {other:?}"),
    }
    obj
}

#[test]
fn whole_session_decrypts() {
    let d = test_key();
    let q = d.public();
    let cryp = checked_object(&cryp_core(&q, &K, &cryp_aux("1581F7TEST00001", 39.9526, -75.1652)));
    let infp10 = checked_object(&infp10_core(&K, &infp10_plain("1581F7TEST00001")));
    let infp11 = checked_object(&infp11_core(&K, "1581F7TEST00001", "TEST-ONLY"));

    let mut dec: Decryptor<4> = Decryptor::new(Some(d));
    // INFP before its CRYP: no session yet.
    assert_eq!(dec.process(&infp10, 0), Err(Error::NoSession));
    let Plain::Cryp(a) = dec.process(&cryp, 10).unwrap() else { panic!() };
    assert_eq!(a.serial_str(), Some("1581F7TEST00001"));
    assert!(a.position_valid());
    assert!((droneid::payload::rad_e7_to_deg(a.lat_raw) - 39.9526).abs() < 1e-5);
    let Plain::Infp10(t) = dec.process(&infp10, 20).unwrap() else { panic!() };
    assert_eq!(t.serial_str(), Some("1581F7TEST00001"));
    assert!(t.has(Infp10::STATE_IN_AIR) && t.has(Infp10::STATE_MOTORS_ON));
    assert!((t.lat() - 39.9526).abs() < 1e-5 && (t.lon() + 75.1652).abs() < 1e-5);
    assert_eq!(t.altitude_m, 120);
    assert_eq!(t.product_type, 84);
    let Plain::Infp11(p) = dec.process(&infp11, 30).unwrap() else { panic!() };
    assert_eq!(p.purpose_str(), Some("TEST-ONLY"));
    assert_eq!(dec.stats.sessions_unwrapped, 1);
    assert_eq!(dec.stats.records_decrypted, 3);
    assert_eq!(dec.sessions().collect::<Vec<_>>(), [SessionKey(K).key_id()]);
}

#[test]
fn wrong_private_key_is_refused() {
    let d = test_key();
    let cryp = checked_object(&cryp_core(&d.public(), &K, &cryp_aux("SN1", 1.0, 2.0)));
    let mut dec: Decryptor<4> = Decryptor::new(Some(test_key()));
    assert_eq!(dec.process(&cryp, 0), Err(Error::C3Mismatch));
    assert_eq!(dec.sessions().count(), 0, "no key kept from a rejected CRYP");
}

#[test]
fn tampering_is_caught() {
    let d = test_key();
    let q = d.public();
    let good = cryp_core(&q, &K, &cryp_aux("SN1", 1.0, 2.0));
    // C2 flipped: the unwrapped key changes, C3 no longer matches.
    let mut c = good.clone();
    c[110] ^= 1;
    assert_eq!(unwrap_cryp(&c, &d).err(), Some(Error::C3Mismatch));
    // C3 flipped.
    let mut c = good.clone();
    c[80] ^= 1;
    assert_eq!(unwrap_cryp(&c, &d).err(), Some(Error::C3Mismatch));
    // Key ID changed: the key is authentic but filed under the wrong ID.
    let mut c = good.clone();
    c[5] ^= 1;
    assert_eq!(unwrap_cryp(&c, &d).err(), Some(Error::KeyIdMismatch));
    // C1 moved off the curve.
    let mut c = good.clone();
    c[40] ^= 1;
    assert_eq!(unwrap_cryp(&c, &d).err(), Some(Error::BadPoint));
    // Wrong lengths and selectors.
    assert_eq!(unwrap_cryp(&good[..100], &d).err(), Some(Error::Shape));
    let mut c = good.clone();
    c[1] = b'X';
    assert_eq!(unwrap_cryp(&c, &d).err(), Some(Error::Shape));
    assert!(unwrap_cryp(&good, &d).is_ok());
}

#[test]
fn infp_under_a_colliding_key_id_fails_its_plaintext_checks() {
    // Another session's key filed under this record's ID (a truncated-hash
    // collision): the plaintext is garbage and the parser rejects it.
    let mut dec: Decryptor<4> = Decryptor::default();
    let other = SessionKey([0x5a; 16]);
    let rec = infp10_core(&K, &infp10_plain("SN2"));
    let (_, obj) = block_and_object(&rec);
    dec.store(SessionKey(K).key_id(), other, 0);
    assert!(matches!(dec.process(&obj, 0), Err(Error::Plain(_))));
}

#[test]
fn hand_supplied_session_key() {
    let mut dec: Decryptor<4> = Decryptor::default();
    assert!(!dec.has_key());
    let (_, obj) = block_and_object(&infp11_core(&K, "SN3", "hello"));
    assert_eq!(dec.process(&obj, 0), Err(Error::NoSession));
    dec.add_session(SessionKey::from_bytes(K), 0);
    assert!(matches!(dec.process(&obj, 1), Ok(Plain::Infp11(_))));
    // A CRYP of that (known) session decrypts with its key: no private
    // key or curve operation needed.
    let d = test_key();
    let (_, cryp) = block_and_object(&cryp_core(&d.public(), &K, &cryp_aux("SN3", 1.0, 2.0)));
    assert!(matches!(dec.process(&cryp, 2), Ok(Plain::Cryp(_))));
    // A CRYP of an unknown session without a private key: detect-only.
    let (_, cryp) = block_and_object(&cryp_core(&d.public(), &[7; 16], &cryp_aux("SN3", 1.0, 2.0)));
    assert_eq!(dec.process(&cryp, 2), Err(Error::NoKey));
    assert_eq!(dec.stats.curve_ops, 0);
}

#[test]
fn sessions_expire_and_the_table_recycles() {
    let mut dec: Decryptor<2> = Decryptor::default();
    for (i, b) in [1u8, 2, 3].iter().enumerate() {
        dec.add_session(SessionKey([*b; 16]), i as u64);
    }
    // Three keys into two slots: the oldest went.
    let ids: Vec<_> = dec.sessions().collect();
    assert_eq!(ids.len(), 2);
    assert!(!ids.contains(&SessionKey([1; 16]).key_id()));
    let (_, obj) = block_and_object(&infp11_core(&[3; 16], "SN4", "x"));
    assert!(dec.process(&obj, 10).is_ok());
    assert_eq!(dec.process(&obj, 10 + SESSION_TTL_MS + 1), Err(Error::NoSession));
}

#[test]
fn key_file_parsing() {
    let hex = "00000000000000000000000000000000000000000000000000000000000000ff";
    assert!(PrivateKey::from_hex_text(hex.as_bytes()).is_ok());
    let commented = std::format!("# O4 key, owner supplied\n0x{}\n", hex.to_uppercase());
    assert!(PrivateKey::from_hex_text(commented.as_bytes()).is_ok());
    let spaced = "0000 0000 0000 0000 0000 0000 0000 0000\n0000 0000 0000 0000 0000 0000 0000 00ff\n";
    assert!(PrivateKey::from_hex_text(spaced.as_bytes()).is_ok());
    assert_eq!(PrivateKey::from_hex_text(&hex.as_bytes()[..62]).err(), Some(KeyError::Length));
    assert_eq!(PrivateKey::from_hex_text(b"zz").err(), Some(KeyError::NotHex));
    let zero = "0".repeat(64);
    assert_eq!(PrivateKey::from_hex_text(zero.as_bytes()).err(), Some(KeyError::OutOfRange));
    let n = "FFFFFFFEFFFFFFFFFFFFFFFFFFFFFFFF7203DF6B21C6052B53BBF40939D54123";
    assert_eq!(PrivateKey::from_hex_text(n.as_bytes()).err(), Some(KeyError::OutOfRange));
}

#[test]
fn the_spec_example_key_round_trips() {
    // SPEC.md "Example keypair": d = 1, so Q = G. Its example session key
    // and IV; our own encryptor plays the aircraft.
    let mut one = [0u8; 32];
    one[31] = 1;
    let d = PrivateKey::from_bytes(&one).unwrap();
    assert_eq!(d.public(), AffinePoint::GENERATOR);
    let k = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    let core = cryp_core(&d.public(), &k, &cryp_aux("DUMMYAIRCRAFT01", 51.5074, -0.1278));
    let s = unwrap_cryp(&core, &d).unwrap();
    let Plain::Cryp(a) = decrypt_core(&core, &s).unwrap() else { panic!() };
    assert_eq!(a.serial_str(), Some("DUMMYAIRCRAFT01"));
}

#[test]
fn the_identity_point_is_refused() {
    // Scalar arithmetic sanity, plus C1 = all zeros is not a curve point.
    assert!(bool::from(Scalar::ZERO.is_zero()));
    let d = test_key();
    let mut c = cryp_core(&d.public(), &K, &cryp_aux("SN5", 1.0, 2.0));
    c[9..73].fill(0);
    assert_eq!(unwrap_cryp(&c, &d).err(), Some(Error::BadPoint));
}

#[test]
fn errors_and_stats_carry_no_key_bytes() {
    // Nothing printable holds key material: errors and stats are plain
    // enums and counters, and the key types have no Debug at all (this
    // would not compile if they did: `std::format!("{:?}", d)`).
    let d = test_key();
    let fp = d.fingerprint();
    let dbg = std::format!("{:?} {:?}", Error::C3Mismatch, Stats::default());
    assert!(!dbg.contains(&std::format!("{:02x}", K[1])));
    assert_ne!(fp, [0; 4]);
}

/// A CRYP repeated by the aircraft (or replayed, or sent again by a
/// forger) costs one curve operation, not one each.
#[test]
fn a_repeated_cryp_costs_one_curve_operation() {
    let d = test_key();
    let q = d.public();
    let cryp = checked_object(&cryp_core(&q, &K, &cryp_aux("1581F7TEST00001", 39.9526, -75.1652)));
    let mut dec: Decryptor<4> = Decryptor::new(Some(d));
    for t in 0..20u64 {
        assert!(matches!(dec.process(&cryp, t * 100), Ok(Plain::Cryp(_))));
    }
    assert_eq!(dec.stats.curve_ops, 1);
    assert_eq!(dec.stats.sessions_unwrapped, 1);
    assert_eq!(dec.stats.records_decrypted, 20);
}

/// CRYPs for new sessions (as a forger with the public key sends them, one
/// new key after another) are unwrapped at most UNWRAP_BURST at once and
/// UNWRAP_PER_S a second.
#[test]
fn unwraps_are_rate_limited() {
    let d = test_key();
    let q = d.public();
    let mut dec: Decryptor<8> = Decryptor::new(Some(d));
    let mut ok = 0;
    for i in 0..20u8 {
        let k = [i; 16];
        let cryp = checked_object(&cryp_core(&q, &k, &cryp_aux("SN1", 1.0, 2.0)));
        match dec.process(&cryp, 10) {
            Ok(_) => ok += 1,
            Err(e) => assert_eq!(e, Error::RateLimited),
        }
    }
    assert_eq!(ok, UNWRAP_BURST);
    assert_eq!(dec.stats.curve_ops, UNWRAP_BURST);
    assert_eq!(dec.stats.rate_limited, 20 - UNWRAP_BURST);
    // One second later: UNWRAP_PER_S more.
    let mut more = 0;
    for i in 100..110u8 {
        let cryp = checked_object(&cryp_core(&q, &[i; 16], &cryp_aux("SN1", 1.0, 2.0)));
        if dec.process(&cryp, 1_010).is_ok() {
            more += 1;
        }
    }
    assert_eq!(more, UNWRAP_PER_S);
}

/// Within a session, an INFP-10 that does not move forward is refused:
/// an old record replayed, an altered (bit-flipped) copy of the latest, GPS
/// time going backwards. An identical repeat (two nodes) is fine. One
/// forged far "ahead" is refused without locking the real records out.
#[test]
fn replayed_and_altered_infp10_records_are_refused() {
    const SN: &str = "1581F7TEST00001";
    let gps0 = 1_794_232_987_250u64;
    let rec = |seq: u16, gps: u64| checked_object(&infp10_core(&K, &infp10_at(SN, seq, gps)));
    let mut dec: Decryptor<4> = Decryptor::default();
    dec.add_session(SessionKey::from_bytes(K), 0);
    assert!(dec.process(&rec(10, gps0), 1_000).is_ok());
    assert!(dec.process(&rec(10, gps0), 1_010).is_ok(), "the same record from a second node");
    assert!(dec.process(&rec(11, gps0 + 200), 1_200).is_ok());
    // Replays of older records.
    assert_eq!(dec.process(&rec(10, gps0), 1_400), Err(Error::Replay));
    assert_eq!(dec.process(&rec(12, gps0 - 1_000), 1_400), Err(Error::Replay), "GPS time backwards");
    // The latest record, its position ciphertext bit-flipped: AES-CTR
    // decrypts it to a moved position that passes the range checks, but it
    // is an altered copy of a record already seen.
    let mut flipped = infp10_core(&K, &infp10_at(SN, 11, gps0 + 200));
    flipped[19 + 28] ^= 0x01;
    let (_, flipped) = block_and_object(&flipped);
    assert_eq!(dec.process(&flipped, 1_500), Err(Error::Replay));
    // A record far "in the future" is refused, and the real next one
    // still decrypts.
    assert_eq!(dec.process(&rec(5_000, gps0 + 200), 1_600), Err(Error::TooFarAhead));
    assert_eq!(dec.process(&rec(12, gps0 + 3_600_000), 1_600), Err(Error::TooFarAhead));
    assert!(dec.process(&rec(12, gps0 + 400), 1_600).is_ok());
    assert_eq!(dec.stats.replayed, 3);
    assert_eq!(dec.stats.too_far_ahead, 2);
    // A bit flip in a *new* record is not detectable here: it decrypts.
    // The tracker judges its position as a claim (docs/SPOOFING.md).
    let mut fresh = infp10_core(&K, &infp10_at(SN, 13, gps0 + 600));
    fresh[19 + 28] ^= 0x01;
    let (_, fresh) = block_and_object(&fresh);
    let Ok(Plain::Infp10(r)) = dec.process(&fresh, 1_800) else { panic!() };
    assert!((r.lat() - 39.9526).abs() > 0.3, "malleable: the latitude moved to {}", r.lat());
}
