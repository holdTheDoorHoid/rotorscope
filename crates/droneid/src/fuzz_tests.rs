//! Fuzz-style tests (crates/fuzzkit) for the DroneID receive chain, from the
//! decoded block back to the samples: the transport-block parser, the O4
//! plaintext records, the turbo decoder fed arbitrary soft bits, and the
//! burst finder and receiver fed arbitrary IQ. Seeds come from the crate's
//! own synthesiser.

use std::vec::Vec;

use crate::math::C16;
use crate::payload::{self, EncKind};
use crate::rx::{Layout, Receiver, Span, find_bursts};
use crate::synth::{Capture, capture, o4_payload, random_payload};
use crate::testutil::Rng;
use crate::payload::BLOCK;
use crate::{D, E, K, o4plain, ratematch, turbo};

fn block(bytes: &[u8]) -> [u8; BLOCK] {
    let mut b = [0u8; BLOCK];
    let n = bytes.len().min(BLOCK);
    b[..n].copy_from_slice(&bytes[..n]);
    b
}

#[test]
fn fuzz_transport_block() {
    let mut r = Rng::new(11);
    let seeds = [random_payload(&mut r), o4_payload(EncKind::Cryp, [1, 2, 3, 4], &mut r), o4_payload(EncKind::Infp10, [1, 2, 3, 4], &mut r), o4_payload(EncKind::Infp11, [1, 2, 3, 4], &mut r)];
    let refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    fuzzkit::run_with("droneid::payload::parse", &fuzzkit::Config::new().max_len(BLOCK), &refs, 8_000, |bytes| {
        let p = payload::parse(&block(bytes));
        let _ = std::format!("{p:?}");
    });
}

#[test]
fn fuzz_o4_plaintext() {
    let i10 = o4plain::build::infp10(&o4plain::build::sample_infp10());
    let i11 = o4plain::build::infp11(b"1581F7TEST00001", "TEST-ONLY");
    let aux = o4plain::build::cryp_aux(b"1581F7TEST00001", 6_972_000, -13_119_000, 1);
    let seeds: [&[u8]; 3] = [&i10, &i11, &aux];
    fuzzkit::run("droneid::o4plain", &seeds, 8_000, |bytes| {
        if let Ok(r) = o4plain::parse_infp10(bytes) {
            let _ = (r.serial_str(), r.identity_str(), r.account_str(), r.lat(), r.lon(), r.height_m(), r.heading_deg());
            // What parsed re-encodes to bytes that parse the same.
            let again = o4plain::build::infp10(&r);
            assert_eq!(o4plain::parse_infp10(&again).ok(), Some(r));
        }
        if let Ok(r) = o4plain::parse_infp11(bytes) {
            let _ = (r.serial_str(), r.purpose_str());
        }
        if let Ok(r) = o4plain::parse_cryp_aux(bytes) {
            let _ = (r.serial_str(), r.position_valid());
        }
        let _ = o4plain::serial_text(bytes);
    });
}

#[test]
fn fuzz_turbo_from_soft_bits() {
    // Soft bits for a real block, then whatever mutation does to them.
    let mut r = Rng::new(12);
    let coded = crate::synth::coded_bits(&random_payload(&mut r));
    let llr: Vec<u8> = coded.iter().map(|&b| if b == 0 { 100u8 } else { 156u8 }).collect();
    let cfg = fuzzkit::Config::new().max_len(E).random_percent(10);
    let mut dec = turbo::Turbo::new();
    fuzzkit::run_with("droneid::turbo::decode", &cfg, &[&llr], 60, |bytes| {
        let mut soft = [0i8; E];
        for (s, &b) in soft.iter_mut().zip(bytes) {
            *s = b as i8;
        }
        let mut d = [[0i16; D]; 3];
        ratematch::derate(&soft, &mut d);
        let mut out = [0u8; K / 8];
        let res = dec.decode(&d, 4, &mut out);
        assert!(res.half_iterations <= 8);
        if res.crc_ok {
            assert!(turbo::crc_ok(&out));
            let _ = payload::parse(&block(&out));
        }
    });
}

#[test]
fn fuzz_receiver_on_iq() {
    // A clean burst at 15.36 MS/s as 16-bit IQ bytes, then mutations of it
    // and random sample buffers. DSP is slow in a debug build: few inputs.
    let fs = 15_360_000;
    let mut r = Rng::new(13);
    let (iq, _) = capture(&random_payload(&mut r), Layout::Nine, &Capture::clean(fs), &mut r);
    let bytes: Vec<u8> = iq.iter().flat_map(|c| [c.re.to_le_bytes(), c.im.to_le_bytes()].concat()).collect();
    let cfg = fuzzkit::Config::new().max_len(bytes.len() + 64).random_percent(20);
    let mut rx = Receiver::new(fs).unwrap();
    fuzzkit::run_with("droneid::rx::Receiver::decode_all", &cfg, &[&bytes], 12, |b| {
        let x: Vec<C16> = b.chunks_exact(4).map(|c| C16 { re: i16::from_le_bytes([c[0], c[1]]), im: i16::from_le_bytes([c[2], c[3]]) }).collect();
        let mut spans = [Span { start: 0, end: 0 }; 8];
        let n = find_bursts(&x, fs, &mut spans);
        for s in &spans[..n.min(spans.len())] {
            assert!(s.start <= s.end && s.end <= x.len());
        }
        rx.decode_all(&x, &mut |res| {
            if let Ok(burst) = res {
                assert!(burst.span.end <= x.len());
                let _ = std::format!("{burst:?}");
            }
        });
    });
}
