//! PLAN.md Phase 13 done-when, host side: a log verifies; editing,
//! deleting, reordering or truncating any record makes the verifier fail
//! and name the place; a cold start with no GPS is visibly marked.

use evidence::format::{self, CheckpointReason, EventCode, Kind};
use evidence::sign::{KeyClass, SoftSigner};
use evidence::verify::{self, Input, Level, Options};
use evidence::writer::{Chain, Context, Detection};
use evidence::{PosSource, Signer, TimeSource};

const FW: [u8; 32] = [0xF1; 32];

fn rid_frame(n: u32, lat: f64) -> link::Frame {
    use rid::message::*;
    let mut basic = [0u8; 25];
    basic[0] = 0x02;
    basic[1] = 0x12;
    basic[2..16].copy_from_slice(b"RSTEST-SD-0001");
    let loc = Message::Location(Location {
        status: Status::Airborne,
        height_type: HeightType::AboveTakeoff,
        direction: Some(90.0),
        speed_horizontal: Some(2.0),
        speed_vertical: Some(0.0),
        position: Some(LatLon { lat, lon: -75.2407 }),
        altitude_pressure: None,
        altitude_geodetic: Some(129.3),
        height: Some(160.0),
        horizontal_accuracy: HorizontalAccuracy::M3,
        vertical_accuracy: VerticalAccuracy::M3,
        baro_accuracy: VerticalAccuracy::M3,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: None,
        timestamp_accuracy: None,
    });
    let mut messages = heapless::Vec::new();
    messages.push(basic).unwrap();
    messages.push(rid::encode::message(&loc).unwrap()).unwrap();
    link::Frame::Detection(link::Detection {
        transport: rid::Transport::WifiBeacon,
        channel: 6,
        rssi: -45,
        mac: [0x02, 0x52, 0x53, 0x0D, 0, 1],
        counter: n as u8,
        uptime_us: u64::from(n) * 1_000_000,
        messages,
    })
}

fn gps_ctx(t: u64) -> Context {
    Context {
        uptime_ms: t,
        utc_ms: Some(1_763_200_000_000 + t),
        time_source: TimeSource::GpsPps,
        time_age_ms: Some(200),
        gps_state: 3,
        pos_source: PosSource::Gps,
        sats: Some(9),
        lat_e7: 399_526_000,
        lon_e7: -751_652_000,
        alt_hae_mm: Some(-12_000),
        hacc_dm: Some(25),
        hdop_x100: Some(90),
        fix_age_ms: Some(300),
    }
}

struct Log {
    chain: Chain,
    signer: SoftSigner,
    bytes: Vec<u8>,
    /// Byte range of each record.
    spans: Vec<(usize, usize)>,
}

impl Log {
    fn new(boot: u8) -> Self {
        Self { chain: Chain::new([boot; 8], FW), signer: SoftSigner::public_test(), bytes: Vec::new(), spans: Vec::new() }
    }
    fn push(&mut self, buf: &[u8], n: usize) {
        self.spans.push((self.bytes.len(), self.bytes.len() + n));
        self.bytes.extend_from_slice(&buf[..n]);
        self.chain.commit();
    }
    fn boot(&mut self, ctx: &Context) {
        let mut data = vec![self.signer.key_class() as u8];
        data.extend_from_slice(&self.signer.public_key());
        data.extend_from_slice(&[1, 3]);
        let mut b = [0u8; format::MAX_RECORD];
        let n = self.chain.event(ctx, EventCode::Boot, b"hub-p4 test", &data, &mut b).unwrap();
        self.push(&b, n);
    }
    fn event(&mut self, ctx: &Context, code: EventCode, text: &str) {
        let mut b = [0u8; format::MAX_RECORD];
        let n = self.chain.event(ctx, code, text.as_bytes(), &[], &mut b).unwrap();
        self.push(&b, n);
    }
    fn detection(&mut self, ctx: &Context, f: &link::Frame) {
        let mut raw = [0u8; link::MAX_FRAME];
        let raw = evidence::summary::raw_bytes(f, &mut raw).unwrap();
        let info = evidence::summary::info(f).unwrap();
        let mut s = evidence::summary::Summary::new();
        evidence::summary::write_summary(f, &mut s).unwrap();
        let d = Detection {
            node: [0x02, 0x52, 0x53, 0, 0, 2],
            link_rssi: Some(-40),
            node_index: 1,
            band: info.band,
            frame_kind: info.frame_kind,
            channel: info.channel,
            freq_100khz: info.freq_100khz,
            rssi: info.rssi,
            raw_format: format::RAW_LINK_FRAME_V6,
            raw,
            summary: s.as_bytes(),
        };
        let mut b = [0u8; format::MAX_RECORD];
        let n = self.chain.detection(ctx, &d, &mut b).unwrap();
        self.push(&b, n);
    }
    fn checkpoint(&mut self, ctx: &Context, reason: CheckpointReason) {
        let mut b = [0u8; format::MAX_RECORD];
        let n = self.chain.checkpoint(ctx, reason, &mut self.signer, &mut b).unwrap();
        self.push(&b, n);
    }
}

/// Boot, a few events and detections, two checkpoints: 12 records.
fn sample() -> Log {
    let mut l = Log::new(7);
    l.boot(&gps_ctx(100));
    l.event(&gps_ctx(200), EventCode::NodeJoin, "node 02:52:53:00:00:02");
    for i in 0..4 {
        l.detection(&gps_ctx(1000 + u64::from(i) * 1000), &rid_frame(i, 39.8721));
    }
    l.checkpoint(&gps_ctx(60_000), CheckpointReason::Periodic);
    l.event(&gps_ctx(61_000), EventCode::Note, "operator note: bench test");
    for i in 4..7 {
        l.detection(&gps_ctx(62_000 + u64::from(i) * 1000), &rid_frame(i, 39.8722));
    }
    l.checkpoint(&gps_ctx(120_000), CheckpointReason::Shutdown);
    l
}

fn check(bytes: &[u8]) -> verify::Report {
    verify::verify(&[Input::new("EV000001.EVL", bytes.to_vec())], &Options::default())
}

fn fails(rep: &verify::Report) -> Vec<String> {
    rep.findings.iter().filter(|f| f.level == Level::Fail).map(|f| format!("{}: {}", f.at, f.what)).collect()
}

#[test]
fn clean_log_verifies() {
    let l = sample();
    assert_eq!(l.spans.len(), 12);
    let rep = check(&l.bytes);
    assert_eq!(rep.exit_code(), 0, "{:#?}", rep.findings);
    let c = &rep.chains[0];
    assert_eq!((c.records, c.detections, c.checkpoints, c.signed_through), (12, 7, 2, Some(11)));
    // A public test key is called out, never silently accepted.
    assert!(rep.findings.iter().any(|f| f.level == Level::Note && f.what.contains("public_test")));
}

#[test]
fn record_layout_matches_the_spec() {
    let l = sample();
    let r = format::parse(&l.bytes).unwrap();
    assert_eq!(&l.bytes[0..4], b"RSEV");
    assert_eq!(l.bytes[4], 1);
    assert_eq!(l.bytes[5], Kind::Event as u8);
    let len = u16::from_le_bytes([l.bytes[6], l.bytes[7]]) as usize;
    assert_eq!(len, l.spans[0].1);
    assert_eq!(u32::from_le_bytes(l.bytes[len - 4..len].try_into().unwrap()) as usize, len);
    assert_eq!(&l.bytes[100..132], &FW);
    assert_eq!(r.header.prev, [0; 32]);
    let second = format::parse(&l.bytes[len..]).unwrap();
    assert_eq!(second.header.prev, evidence::sha256(&l.bytes[..len]));
    assert_eq!(second.header.seq, 1);
}

#[test]
fn editing_any_byte_of_any_record_fails_and_names_it() {
    let l = sample();
    for (i, &(a, b)) in l.spans.iter().enumerate() {
        // Every byte would take a while; a spread of positions in each record
        // (header, body, signature, trailer).
        for pos in [a, a + 9, a + 30, a + 60, a + 70, a + 80, a + 101, a + 140, (a + b) / 2, b - 70, b - 5, b - 1] {
            let mut t = l.bytes.clone();
            t[pos] ^= 0x01;
            let rep = check(&t);
            let f = fails(&rep);
            assert!(!f.is_empty(), "edit at byte {pos} (record #{i}) not caught");
            assert_eq!(rep.exit_code(), 1);
            // The report names this record or the next (whose link breaks),
            // or the byte offset of the damaged record.
            let named = rep.findings.iter().any(|f| {
                f.level == Level::Fail
                    && (f.at.seq == Some(i as u64) || f.at.seq == Some(i as u64 + 1) || f.at.offset == Some(a))
            });
            assert!(named, "edit at byte {pos} (record #{i}): {f:?}");
        }
    }
}

#[test]
fn deleting_any_record_fails() {
    let l = sample();
    for (i, &(a, b)) in l.spans.iter().enumerate() {
        let mut t = l.bytes.clone();
        t.drain(a..b);
        let rep = check(&t);
        if i == l.spans.len() - 1 {
            // The very last record of a file looks like a log that simply
            // stopped: not provable from the file alone, but the records are
            // then no longer covered by a signature.
            assert_eq!(rep.exit_code(), 3, "{:?}", rep.findings);
            continue;
        }
        let f = fails(&rep);
        assert!(!f.is_empty(), "deleting record #{i} not caught");
        assert!(f.iter().any(|s| s.contains(&format!("#{}", i + 1)) || s.contains(&format!("#{i}"))), "#{i}: {f:?}");
    }
}

#[test]
fn reordering_records_fails() {
    let l = sample();
    for i in 0..l.spans.len() - 1 {
        let (a, m) = l.spans[i];
        let (_, b) = l.spans[i + 1];
        let mut t = l.bytes[..a].to_vec();
        t.extend_from_slice(&l.bytes[m..b]);
        t.extend_from_slice(&l.bytes[a..m]);
        t.extend_from_slice(&l.bytes[b..]);
        let rep = check(&t);
        assert!(!fails(&rep).is_empty(), "swapping #{i} and #{} not caught", i + 1);
    }
}

#[test]
fn truncating_a_record_inside_the_log_fails() {
    let l = sample();
    for (i, &(a, b)) in l.spans.iter().enumerate().take(l.spans.len() - 1) {
        // Cut the record's second half out; the rest of the log stays.
        let mut t = l.bytes.clone();
        t.drain((a + b) / 2..b);
        let rep = check(&t);
        assert_eq!(rep.exit_code(), 1, "cutting record #{i}: {:?}", rep.findings);
    }
    // Cutting the end of the file: reported as an incomplete last record,
    // and the records after the last checkpoint are no longer signed.
    let mut t = l.bytes.clone();
    t.truncate(l.spans[11].0 + 50);
    let rep = check(&t);
    assert_eq!(rep.exit_code(), 3);
    assert!(rep.findings.iter().any(|f| f.what.contains("incomplete record") && f.at.offset == Some(l.spans[11].0)));
    assert!(rep.findings.iter().any(|f| f.what.contains("after the last signed checkpoint")));
}

#[test]
fn rewriting_the_chain_after_an_edit_breaks_the_signature() {
    // An attacker edits record 3's summary and recomputes every later hash.
    // Without the key the checkpoint signature no longer matches.
    let l = sample();
    let mut recs: Vec<Vec<u8>> = l.spans.iter().map(|&(a, b)| l.bytes[a..b].to_vec()).collect();
    let r3 = &mut recs[3];
    let n = r3.len();
    r3[n - 10] ^= 0x20;
    for i in 4..recs.len() {
        let h = evidence::sha256(&recs[i - 1]);
        recs[i][24..56].copy_from_slice(&h);
    }
    let t: Vec<u8> = recs.concat();
    let rep = check(&t);
    let f = fails(&rep);
    assert!(f.iter().any(|s| s.contains("record #6") && s.contains("signature")), "{f:?}");
    // And the summary no longer matches the raw bytes either.
    assert!(f.iter().any(|s| s.contains("record #3")), "{f:?}");
}

#[test]
fn cold_start_without_gps_is_marked() {
    let mut l = Log::new(9);
    let cold = Context { uptime_ms: 50, ..Context::default() };
    l.boot(&cold);
    l.detection(&Context { uptime_ms: 900, gps_state: 1, ..Context::default() }, &rid_frame(1, 39.8));
    l.checkpoint(&Context { uptime_ms: 60_000, ..Context::default() }, CheckpointReason::Periodic);
    // The bytes say it plainly: UTC 0, time source 0, position source 0.
    let r = format::parse(&l.bytes[l.spans[1].0..]).unwrap();
    assert_eq!(&l.bytes[l.spans[1].0 + 64..l.spans[1].0 + 72], &[0u8; 8]);
    assert_eq!(r.header.time_source, TimeSource::None as u8);
    assert_eq!(r.header.pos_source, PosSource::None as u8);
    assert_eq!(r.header.gps_state, 1);
    // A time source cannot be claimed without a UTC value.
    let mut c = Chain::new([1; 8], FW);
    let mut b = [0u8; 1024];
    let n = c.event(&Context { time_source: TimeSource::GpsPps, ..Context::default() }, EventCode::Boot, b"", &[], &mut b).unwrap();
    assert_eq!(format::parse(&b[..n]).unwrap().header.time_source, 0);

    let rep = check(&l.bytes);
    assert_eq!(rep.exit_code(), 0, "{:?}", rep.findings);
    assert!(rep.findings.iter().any(|f| f.what.contains("3 records have no UTC time")));
    assert!(rep.findings.iter().any(|f| f.what.contains("no hub position")));
    let line = verify::describe(&r);
    assert!(line.contains("NO-UTC") && line.contains("NO-POSITION(gps searching)"), "{line}");
}

#[test]
fn card_files_link_to_each_other() {
    // One boot, two card files: the second opens with a card-mounted event
    // naming the first file's length and last record.
    let l = sample();
    let first = l.bytes[..l.spans[6].1].to_vec();
    let last = format::last_complete(&first).unwrap();
    assert_eq!(last, (l.spans[6].0, false));
    let mut l2 = Log { chain: l.chain, signer: l.signer, bytes: Vec::new(), spans: Vec::new() };
    // Rebuild a chain that continues from record 6 for the second file.
    let mut c = Chain::new([7; 8], FW);
    let mut buf = [0u8; 1024];
    let recs: Vec<&[u8]> = l.spans[..7].iter().map(|&(a, b)| &l.bytes[a..b]).collect();
    for r in &recs {
        // Replay to bring the chain to the same head.
        let p = format::parse(r).unwrap();
        assert_eq!(p.header.prev, c.head());
        let _ = &mut buf;
        c = replay(c, r);
    }
    l2.chain = c;
    let mut data = Vec::new();
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&(first.len() as u32).to_le_bytes());
    data.extend_from_slice(&evidence::sha256(recs[6]));
    data.push(1);
    let mut b = [0u8; 1024];
    let n = l2.chain.event(&gps_ctx(70_000), EventCode::CardMounted, b"EV000002.EVL", &data, &mut b).unwrap();
    l2.push(&b, n);
    l2.checkpoint(&gps_ctx(71_000), CheckpointReason::Periodic);
    let files = vec![Input::new("EV000001.EVL", first.clone()), Input::new("EV000002.EVL", l2.bytes.clone())];
    let rep = verify::verify(&files, &Options::default());
    assert_eq!(rep.exit_code(), 0, "{:?}", rep.findings);

    // Records appended to the first file afterwards are caught...
    let mut grown = first.clone();
    grown.extend_from_slice(&l.bytes[l.spans[7].0..l.spans[7].1]);
    let rep = verify::verify(&[Input::new("EV000001.EVL", grown), Input::new("EV000002.EVL", l2.bytes.clone())], &Options::default());
    assert!(fails(&rep).iter().any(|s| s.contains("added to")), "{:?}", rep.findings);
    // ...and so is cutting the first file's last record.
    let cut = first[..l.spans[6].0].to_vec();
    let rep = verify::verify(&[Input::new("EV000001.EVL", cut), Input::new("EV000002.EVL", l2.bytes.clone())], &Options::default());
    assert!(fails(&rep).iter().any(|s| s.contains("cut short")), "{:?}", rep.findings);
    // A missing file in between is reported.
    let rep = verify::verify(&[Input::new("EV000001.EVL", first), Input::new("EV000003.EVL", l2.bytes)], &Options::default());
    assert!(rep.findings.iter().any(|f| f.what.contains("not supplied")));
}

/// Advance a fresh chain over an existing record (same boot id and firmware).
fn replay(mut c: Chain, rec: &[u8]) -> Chain {
    // Chain has no public setter; rebuild an identical record and commit it.
    let p = format::parse(rec).unwrap();
    let mut b = [0u8; 1024];
    let ctx = Context {
        uptime_ms: p.header.uptime_ms,
        utc_ms: p.header.utc_ms,
        time_source: TimeSource::from_u8(p.header.time_source).unwrap(),
        time_age_ms: p.header.time_age_ms,
        gps_state: p.header.gps_state,
        pos_source: PosSource::from_u8(p.header.pos_source).unwrap(),
        sats: p.header.sats,
        lat_e7: p.header.lat_e7,
        lon_e7: p.header.lon_e7,
        alt_hae_mm: p.header.alt_hae_mm,
        hacc_dm: p.header.hacc_dm,
        hdop_x100: p.header.hdop_x100,
        fix_age_ms: p.header.fix_age_ms,
    };
    let n = match p.body {
        format::Body::Event(e) => c.event(&ctx, EventCode::from_u16(e.code).unwrap(), e.text, e.data, &mut b).unwrap(),
        format::Body::Detection(d) => c
            .detection(
                &ctx,
                &Detection {
                    node: d.node,
                    link_rssi: d.link_rssi,
                    node_index: d.node_index,
                    band: d.band,
                    frame_kind: d.frame_kind,
                    channel: d.channel,
                    freq_100khz: d.freq_100khz,
                    rssi: d.rssi,
                    raw_format: d.raw_format,
                    raw: d.raw,
                    summary: d.summary,
                },
                &mut b,
            )
            .unwrap(),
        format::Body::Checkpoint(_) => {
            let mut s = SoftSigner::public_test();
            c.checkpoint(&ctx, CheckpointReason::Periodic, &mut s, &mut b).unwrap()
        }
    };
    assert_eq!(&b[..n], rec, "replay must reproduce the record exactly");
    c.commit();
    c
}

#[test]
fn uncommitted_records_leave_no_hole() {
    let mut c = Chain::new([3; 8], FW);
    let mut b = [0u8; 1024];
    c.event(&Context::default(), EventCode::Boot, b"", &[], &mut b).unwrap();
    c.commit();
    // Built but not kept (no room): the chain does not move.
    c.event(&Context::default(), EventCode::Note, b"dropped", &[], &mut b).unwrap();
    assert_eq!(c.seq(), 1);
    let n = c.event(&Context::default(), EventCode::Note, b"kept", &[], &mut b).unwrap();
    assert_eq!(format::parse(&b[..n]).unwrap().header.seq, 1);
}

#[test]
fn signatures_are_standard_ecdsa_p256_sha256() {
    let l = sample();
    let (a, b) = l.spans[6];
    let r = format::parse(&l.bytes[a..b]).unwrap();
    let format::Body::Checkpoint(cp) = r.body else { panic!() };
    // The digest is plain SHA-256 of the bytes before the signature, so any
    // ECDSA library can check it; the public key is SEC1 uncompressed.
    assert_eq!(cp.pubkey[0], 0x04);
    assert_eq!(cp.signed.len(), b - a - 4 - 64);
    assert!(evidence::sign::verify_digest(&cp.pubkey, &evidence::sha256(cp.signed), &cp.signature));
    assert_eq!(cp.note, KeyClass::PublicTest.note().as_bytes());
    let mut dev = SoftSigner::new(&[0x11; 32], KeyClass::DevelopmentSoftware).unwrap();
    assert_eq!(dev.key_class(), KeyClass::DevelopmentSoftware);
    let sig = dev.sign_digest(&[0x22; 32]).unwrap();
    assert!(evidence::sign::verify_digest(&dev.public_key(), &[0x22; 32], &sig));
    assert!(!evidence::sign::verify_digest(&dev.public_key(), &[0x23; 32], &sig));
}

#[test]
fn file_numbers() {
    assert_eq!(verify::file_number("/media/x/ROTORSCP/EVIDENCE/EV000012.EVL"), Some(12));
    assert_eq!(verify::file_number("ev000003.evl"), Some(3));
    assert_eq!(verify::file_number("EV12.EVL"), None);
}

/// Protocol 11: an aero node's raw aircraft frame is a data frame: logged
/// as kind AIRCRAFT on the RF band with its frequency, and its stored bytes
/// decode back to the same frame and summary (what the verifier checks).
#[test]
fn aircraft_frames_are_logged_raw() {
    let mut raw = heapless::Vec::new();
    raw.extend_from_slice(&[0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98]).unwrap();
    let f = link::Frame::Air(link::AirFrame {
        proto: link::AirProto::Es1090,
        freq_khz: 1_090_000,
        rssi: -71,
        utc_ms: Some(1_763_337_600_123),
        uptime_us: 5_000_000,
        raw,
    });
    let info = evidence::summary::info(&f).unwrap();
    assert_eq!(info.frame_kind, evidence::format::frame_kind::AIRCRAFT);
    assert_eq!(info.band, evidence::format::band::RF);
    assert_eq!(info.freq_100khz, 10_900);
    assert_eq!(info.rssi, Some(-71));
    let mut s = evidence::summary::Summary::new();
    evidence::summary::write_summary(&f, &mut s).unwrap();
    assert_eq!(
        core::str::from_utf8(s.as_bytes()).unwrap(),
        "aircraft es1090 f=1090.000MHz rssi-71 raw=14B node_utc_ms=1763337600123"
    );
    let mut buf = [0u8; link::MAX_FRAME];
    let bytes = evidence::summary::raw_bytes(&f, &mut buf).unwrap().to_vec();
    let (i2, s2, f2) = evidence::summary::decode_raw(evidence::format::RAW_LINK_FRAME_V6, &bytes).unwrap();
    assert_eq!((i2, s2.as_bytes(), f2), (info, s.as_bytes(), f));
    // AeroStatus is bookkeeping, not a detection.
    let st = link::Frame::AeroStatus(link::AeroStatus {
        mode: 9_780,
        freq_khz: 978_000,
        time: link::AeroTime::Hub,
        parked: false,
        radio_ok: true,
        rx_restarts: 0,
        rejected: 0,
        rate_dropped: 0,
    });
    assert!(evidence::summary::info(&st).is_none());
}

/// Protocol 12: control-link frames are data frames, kinds CONTROL and
/// CONTROL_ENERGY on the RF band; the stored bytes decode back to the same
/// frame and summary, and an ID (opt-in) stays out of the summary.
#[test]
fn control_frames_are_logged_raw() {
    use link::ctrlwatch::{self, content::Channels, Confidence, ControlContent, LinkId, Probe, Region, System};
    let mut ch = Channels::default();
    ch.set(0, 1500);
    ch.set(1, 1500);
    let f = link::Frame::Control(link::ControlReport {
        sighting: ctrlwatch::ControlSighting {
            system: System::ExpressLrs,
            region: Region::Fcc915,
            probe: Probe::Elrs { rate: ctrlwatch::elrs::ElrsRate::L900_200, version: ctrlwatch::elrs::OtaVersion::V4 },
            freq_hz: 915_500_000,
            rssi_dbm: -70,
            snr_db: 6,
            t_us: 7,
            confidence: Confidence::Confirmed,
            direction: ctrlwatch::Direction::Uplink,
            id: Some(LinkId::ElrsUid { uid3: None, uid4: 0xAB, uid5: 0xCD, uid5_real: 0xCD }),
            version: Some(4),
        },
        best: Confidence::Confirmed,
        packets: 40,
        confirmed: 2,
        peak_rssi: -60,
        part: 1,
        content: [ControlContent::Channels(ch)].into_iter().collect(),
        raw: Some([1u8, 2, 3, 4, 5, 6, 7, 8].into_iter().collect()),
    });
    let info = evidence::summary::info(&f).unwrap();
    assert_eq!((info.frame_kind, info.band, info.freq_100khz, info.rssi), (evidence::format::frame_kind::CONTROL, evidence::format::band::RF, 9155, Some(-70)));
    let mut s = evidence::summary::Summary::new();
    evidence::summary::write_summary(&f, &mut s).unwrap();
    let text = core::str::from_utf8(s.as_bytes()).unwrap();
    assert_eq!(text, "control ExpressLrs Fcc915 f=915.500MHz rssi-70 snr6 Confirmed best=Confirmed part=1 pkts=40 id=yes channels=2 raw=8B");
    assert!(!text.contains("ab") && !text.contains("cd"), "no ID bytes in the summary");
    let mut buf = [0u8; link::MAX_FRAME];
    let bytes = evidence::summary::raw_bytes(&f, &mut buf).unwrap().to_vec();
    let (i2, s2, f2) = evidence::summary::decode_raw(evidence::format::RAW_LINK_FRAME_V6, &bytes).unwrap();
    assert_eq!((i2, s2.as_bytes(), f2), (info, s.as_bytes(), f));

    let e = link::Frame::ControlEnergy(link::ControlEnergy {
        region: Region::Ism2g4,
        windows: 25,
        activity: link::EnergyActivity::Hopper(link::HopperReport {
            low_khz: 2_400_500,
            high_khz: 2_479_500,
            channels: 30,
            spacing_khz: Some(1_000),
            occupied_khz: 1_000,
            rssi: -66,
            spread_db: 4,
            burst_us: None,
            interval_us: None,
            periodic_pct: 0,
            candidates: [(System::ExpressLrs, 60)].into_iter().collect(),
        }),
        steady: heapless::Vec::new(),
        uptime_us: 1,
    });
    let info = evidence::summary::info(&e).unwrap();
    assert_eq!((info.frame_kind, info.freq_100khz), (evidence::format::frame_kind::CONTROL_ENERGY, 24_000));
    let mut s = evidence::summary::Summary::new();
    evidence::summary::write_summary(&e, &mut s).unwrap();
    assert_eq!(core::str::from_utf8(s.as_bytes()).unwrap(), "control_energy Ism2g4 windows=25 steady=0 hopper 2400500-2479500kHz ch30 rssi-66 best=ExpressLrs:60");
    let bytes = evidence::summary::raw_bytes(&e, &mut buf).unwrap().to_vec();
    assert!(evidence::summary::decode_raw(evidence::format::RAW_LINK_FRAME_V6, &bytes).is_ok());
}
