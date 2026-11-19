//! Fuzz-style tests (crates/fuzzkit) for the O4 DroneID decryptor: object
//! and core shapes, the SM2 session-key unwrap and AES-CTR records, under a
//! private key made up here (as in tests.rs; never a real key). The seeds
//! are a whole session: CRYP, INFP-10 and INFP-11 objects.

extern crate std;

use std::vec::Vec;

use sm2::NonZeroScalar;

use super::*;
use crate::tests::{K, checked_object, cryp_aux, cryp_core_wrapped, infp10_core, infp10_plain, infp11_core, wrap_with};

fn fixed_key() -> PrivateKey {
    PrivateKey::from_bytes(&[0x42; 32]).expect("a valid scalar")
}

fn session() -> Vec<Vec<u8>> {
    let d = fixed_key();
    let e = NonZeroScalar::try_from(&[0x07u8; 32][..]).expect("a valid scalar");
    let wrapped = wrap_with(&d.public(), &K, e);
    std::vec![
        checked_object(&cryp_core_wrapped(&K, &cryp_aux("1581F7TEST00001", 39.9526, -75.1652), wrapped)),
        checked_object(&infp10_core(&K, &infp10_plain("1581F7TEST00001"))),
        checked_object(&infp11_core(&K, "1581F7TEST00001", "TEST-ONLY")),
    ]
}

#[test]
fn fuzz_o4_objects() {
    let seeds = session();
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let cfg = fuzzkit::Config::new().max_len(200);
    fuzzkit::run_with("djicrypt::Decryptor::process", &cfg, &refs, 400, |bytes| {
        if let Ok(core) = core_of(bytes) {
            let (o, i) = (bytes.as_ptr_range(), core.as_ptr_range());
            assert!(o.start <= i.start && i.end <= o.end);
            let _ = key_id_of(core);
            let _ = decrypt_core(core, &SessionKey::from_bytes(K));
        }
        // A decryptor that already holds the session (records decrypt), and
        // one that holds only the private key (a CRYP is unwrapped: the slow
        // SM2 path, so fewer iterations here).
        let mut with_session: Decryptor<2> = Decryptor::new(None);
        with_session.add_session(SessionKey::from_bytes(K), 0);
        let _ = with_session.process(bytes, 10);
        let mut cold: Decryptor<2> = Decryptor::new(Some(fixed_key()));
        let _ = cold.process(bytes, 10);
    });
}

#[test]
fn fuzz_key_text() {
    let text = b"# O4 key\n4242424242424242424242424242424242424242424242424242424242424242\n";
    fuzzkit::run("djicrypt::PrivateKey::from_hex_text", &[text], 3_000, |bytes| {
        if let Ok(k) = PrivateKey::from_hex_text(bytes) {
            let _ = k.fingerprint();
        }
    });
}

/// The whole seed session decrypts, so mutations start from the deep path.
#[test]
fn seed_session_decrypts() {
    let s = session();
    let mut d: Decryptor<2> = Decryptor::new(Some(fixed_key()));
    assert!(matches!(d.process(&s[0], 0), Ok(Plain::Cryp(_))));
    assert!(matches!(d.process(&s[1], 10), Ok(Plain::Infp10(_))));
    assert!(matches!(d.process(&s[2], 20), Ok(Plain::Infp11(_))));
}
