//! Fuzz-style tests (crates/fuzzkit) for every byte parser in this crate:
//! the node link stream, the command stream, one-chunk decoding, a node's
//! batch, the gateway wire both ways and the text cleaner.

use std::string::String as StdString;
use std::vec::Vec as StdVec;

use crate::gw::{self, GwDecoder, GwDown, GwUp, MAX_GW_FRAME};
use crate::origin::{self, NodeChunk, Sorted, Sorter};
use crate::printable::{JsonPrintable, Printable};
use crate::{Command, CommandDecoder, Decoder, Frame, LinkError, MAX_FRAME, encode, encode_command};

const ITERS: usize = 6_000;

fn frame_seeds() -> StdVec<StdVec<u8>> {
    let mut out = StdVec::new();
    let mut buf = [0u8; 4 * MAX_FRAME];
    let samples = crate::tests::samples();
    for f in &samples {
        if let Ok(b) = encode(f, &mut buf) {
            out.push(b.to_vec());
        }
    }
    // A few frames back to back, with resync zeros between two of them.
    let mut joined = StdVec::new();
    for s in out.iter().take(4) {
        joined.extend_from_slice(s);
    }
    joined.extend_from_slice(&[0, 0]);
    joined.extend_from_slice(&out[out.len() - 1]);
    out.push(joined);
    out
}

fn command_seeds() -> StdVec<StdVec<u8>> {
    let cmds = [
        Command::To { node: [1, 2, 3, 4, 5, 6] },
        Command::Poll { addr: 3, epoch: u16::MAX, ack: 255, budget: u16::MAX },
        Command::JoinSlot { epoch: 7, slots: 8, slot_us: 500 },
        Command::Address { mac: [0xFF; 6], addr: 9, epoch: 1, check: 0xBEEF },
        Command::TimeSync { utc_ms: u64::MAX, pps: true },
    ];
    let mut buf = [0u8; MAX_FRAME];
    cmds.iter().map(|c| encode_command(c, &mut buf).unwrap().to_vec()).collect()
}

fn as_refs(v: &[StdVec<u8>]) -> StdVec<&[u8]> {
    v.iter().map(|s| s.as_slice()).collect()
}

/// Re-encoding a decoded frame works, and decoding that gives the same bytes
/// again (bytes, not values: a frame may hold a NaN).
fn frame_round_trips(f: &Frame) {
    let mut a = [0u8; 4 * MAX_FRAME];
    let mut b = [0u8; 4 * MAX_FRAME];
    let first = encode(f, &mut a).expect("a decoded frame re-encodes").to_vec();
    let back = crate::origin::decode_chunk(&first);
    // A decoded frame can re-encode longer than MAX_FRAME (COBS overhead);
    // then decode_chunk refuses it, which is fine. Otherwise it must agree.
    if let Some(back) = back {
        let second = encode(&back, &mut b).unwrap();
        assert_eq!(first, second);
    } else {
        assert!(first.len() > MAX_FRAME, "a re-encoded frame of {} bytes did not decode", first.len());
    }
}

fn decode_stream(bytes: &[u8], split: usize) -> StdVec<Result<StdVec<u8>, LinkError>> {
    let mut d = Decoder::new();
    let mut out = StdVec::new();
    let mut buf = [0u8; 4 * MAX_FRAME];
    let mut push = |r: Result<Frame, LinkError>| {
        out.push(r.map(|f| {
            frame_round_trips(&f);
            encode(&f, &mut buf).unwrap().to_vec()
        }))
    };
    if split == 0 {
        d.feed(bytes, &mut push);
    } else {
        for c in bytes.chunks(split) {
            d.feed(c, &mut push);
        }
    }
    out
}

#[test]
fn fuzz_frame_stream() {
    let seeds = frame_seeds();
    fuzzkit::run_with("link::Decoder", &fuzzkit::Config::new().max_len(1400), &as_refs(&seeds), ITERS, |bytes| {
        let whole = decode_stream(bytes, 0);
        // How the bytes are split across calls changes nothing.
        let split = 1 + (bytes.len() % 7);
        assert_eq!(whole, decode_stream(bytes, split), "result depends on the split");
        // Every frame (or error) needs at least one delimiter.
        let zeros = bytes.iter().filter(|&&b| b == 0).count();
        let oks = whole.iter().filter(|r| r.is_ok()).count();
        assert!(oks <= zeros);
    });
}

#[test]
fn fuzz_frame_stream_through_sorter_and_forwarder() {
    let seeds = frame_seeds();
    fuzzkit::run("link::Sorter+Forwarder", &as_refs(&seeds), ITERS / 2, |bytes| {
        let mut d = Decoder::new();
        let mut s = Sorter::new();
        let mut hub = Sorter::hub_link();
        let mut fw = crate::forward::Forwarder::<1024>::new();
        d.feed(bytes, |r| match r {
            Ok(f) => {
                if let Sorted::Relayed { .. } = s.sort(&f) {
                    assert!(origin::node_may_send(&f));
                }
                let _ = hub.sort(&f);
                fw.relay(&f);
            }
            Err(_) => {
                s.reset();
                hub.reset();
                fw.reset_via();
            }
        });
        // The forwarder's queue holds whole frames only: every queued byte
        // run ends in a delimiter, and each frame decodes.
        assert!(fw.len() <= 1024);
        let q = fw.peek().to_vec();
        if !q.is_empty() {
            assert_eq!(q.last(), Some(&0));
        }
        let n = fw.len();
        fw.consume(n);
        assert!(fw.is_empty());
    });
}

#[test]
fn fuzz_command_stream() {
    let seeds = command_seeds();
    fuzzkit::run("link::CommandDecoder", &as_refs(&seeds), ITERS, |bytes| {
        let mut d = CommandDecoder::new();
        let mut buf = [0u8; MAX_FRAME];
        d.feed(bytes, |r| {
            if let Ok(c) = r {
                let again = encode_command(&c, &mut buf).expect("a decoded command re-encodes");
                let mut d2 = CommandDecoder::new();
                let mut got = None;
                d2.feed(again, |r| got = Some(r));
                assert_eq!(got, Some(Ok(c)));
            }
        });
    });
}

#[test]
fn fuzz_decode_chunk_and_node_batch() {
    let seeds = frame_seeds();
    fuzzkit::run("link::origin::node_batch", &as_refs(&seeds), ITERS, |bytes| {
        if let Some(f) = origin::decode_chunk(bytes) {
            frame_round_trips(&f);
            assert_eq!(origin::origin_of_encoded(bytes), Some(origin::origin(&f)));
        } else {
            let _ = origin::origin_of_encoded(bytes);
        }
        let _ = origin::head_of_encoded(bytes);
        let range = bytes.as_ptr_range();
        origin::node_batch(bytes, |c| match c {
            NodeChunk::Relay { bytes: b, frame } => {
                // The relayed bytes lie inside the payload and end in a delimiter.
                let r = b.as_ptr_range();
                assert!(range.start <= r.start && r.end <= range.end);
                assert_eq!(b.last(), Some(&0));
                assert!(origin::node_may_send(&frame));
            }
            NodeChunk::Refused { frame, origin: o } => {
                assert!(!origin::node_may_send(&frame));
                assert_eq!(o, origin::origin(&frame));
            }
            NodeChunk::Corrupt => {}
        });
    });
}

fn gw_seeds() -> (StdVec<StdVec<u8>>, StdVec<StdVec<u8>>) {
    let mut buf = [0u8; MAX_GW_FRAME];
    let ups = gw::tests::ups().iter().map(|m| gw::encode_up(m, &mut buf).unwrap().to_vec()).collect();
    let downs = gw::tests::downs().iter().map(|m| gw::encode_down(m, &mut buf).unwrap().to_vec()).collect();
    (ups, downs)
}

#[test]
fn fuzz_gateway_up() {
    let (ups, _) = gw_seeds();
    let cfg = fuzzkit::Config::new().max_len(2600);
    fuzzkit::run_with("link::gw::feed_up", &cfg, &as_refs(&ups), ITERS / 2, |bytes| {
        let mut store = [0u8; MAX_GW_FRAME];
        let mut d = GwDecoder::new(&mut store);
        let mut out = [0u8; MAX_GW_FRAME];
        let mut n = 0;
        d.feed_up(bytes, |r| {
            if let Ok(m) = r {
                n += 1;
                assert!(m.fits(), "an accepted message is within the wire's limits");
                let again = gw::encode_up(&m, &mut out).expect("an accepted message re-encodes").to_vec();
                let want: StdString = std::format!("{m:?}");
                let mut store2 = [0u8; MAX_GW_FRAME];
                let mut d2 = GwDecoder::new(&mut store2);
                let mut got = StdVec::new();
                d2.feed_up(&again, |r| got.push(r.map(|m: GwUp<'_>| std::format!("{m:?}"))));
                assert_eq!(got, std::vec![Ok(want)]);
            }
        });
        assert!(n <= bytes.iter().filter(|&&b| b == 0).count());
    });
}

#[test]
fn fuzz_gateway_down() {
    let (_, downs) = gw_seeds();
    let cfg = fuzzkit::Config::new().max_len(2600);
    fuzzkit::run_with("link::gw::feed_down", &cfg, &as_refs(&downs), ITERS / 2, |bytes| {
        // A short buffer too: longer messages must be refused, not overrun.
        for cap in [MAX_GW_FRAME, 64] {
            let mut store = std::vec![0u8; cap];
            let mut d = GwDecoder::new(store.as_mut_slice());
            let mut out = [0u8; MAX_GW_FRAME];
            d.feed_down(bytes, |r| {
                if let Ok(m) = r {
                    assert!(m.fits());
                    let again = gw::encode_down(&m, &mut out).expect("re-encodes").to_vec();
                    let want: StdString = std::format!("{m:?}");
                    let mut store2 = [0u8; MAX_GW_FRAME];
                    let mut d2 = GwDecoder::new(&mut store2);
                    let mut got = StdVec::new();
                    d2.feed_down(&again, |r| got.push(r.map(|m: GwDown<'_>| std::format!("{m:?}"))));
                    assert_eq!(got, std::vec![Ok(want)]);
                }
            });
        }
    });
}

#[test]
fn fuzz_printable() {
    let seeds: [&[u8]; 3] = [b"DJI-Mavic \x1b[2J\r\n", "caf\u{e9} \"x\\y\"".as_bytes(), b"plain"];
    fuzzkit::run("link::printable", &seeds, ITERS, |bytes| {
        let s = fuzzkit::text(bytes);
        for out in [std::format!("{}", Printable(&s)), std::format!("{}", JsonPrintable(&s))] {
            assert!(out.bytes().all(|b| (b' '..=b'~').contains(&b)), "{out:?}");
            assert_eq!(out.chars().count(), s.chars().count());
        }
        assert!(!std::format!("{}", JsonPrintable(&s)).contains(['"', '\\']));
    });
}
