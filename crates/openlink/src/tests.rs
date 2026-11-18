//! Hand-built frames for every system, the ordinary traffic that must not
//! match, and the node's sighting table.

use super::*;

/// Four bytes standing in for the FCS slot the ESP32 callback includes.
const FCS: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

/// A 24-byte header with the given frame control and addresses, then `body`,
/// then the FCS slot.
fn frame(fc: [u8; 2], a1: [u8; 6], a2: [u8; 6], a3: [u8; 6], body: &[u8]) -> Vec<u8> {
    let mut f = vec![fc[0], fc[1], 0x00, 0x00];
    f.extend_from_slice(&a1);
    f.extend_from_slice(&a2);
    f.extend_from_slice(&a3);
    f.extend_from_slice(&[0x30, 0x12]); // sequence control
    f.extend_from_slice(body);
    f.extend_from_slice(&FCS);
    f
}

/// A wfb-ng frame exactly as `tx.cpp` builds it.
fn wfb(fc0: u8, link_id: u32, port: u8, packet_type: u8, body_len: usize) -> Vec<u8> {
    let ch = ((link_id << 8) | u32::from(port)).to_be_bytes();
    let mac = [0x57, 0x42, ch[0], ch[1], ch[2], ch[3]];
    let mut body = vec![0xA5; body_len];
    body[0] = packet_type;
    frame([fc0, 0x01], [0xFF; 6], mac, mac, &body)
}

/// An OpenHD frame as `WBTxRx::tx_inject_packet` builds it.
fn openhd(unique_id: u8, nonce: u64, radio_port: u8) -> Vec<u8> {
    let n = nonce.to_le_bytes();
    let a2 = [unique_id, n[0], n[1], n[2], n[3], radio_port];
    let a3 = [unique_id, n[4], n[5], n[6], n[7], radio_port];
    frame([0x08, 0x01], [0xFF; 6], a2, a3, &[0x5A; 64])
}

/// A RubyFPV frame: encoded port in A1, fixed A2/A3, one `t_packet_header`.
fn ruby(port: u8, flags: u8, src: u32, dest: u32, extra: usize) -> Vec<u8> {
    let a1 = [(port << 4) | 0x0F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    let total = (24 + extra) as u16;
    let mut h = Vec::new();
    h.extend_from_slice(&0x0012_3456u32.to_le_bytes()); // uCRC, top byte 0
    h.push(flags);
    h.push(1); // packet_type
    h.extend_from_slice(&0x4000_0007u32.to_le_bytes()); // stream_packet_idx
    h.extend_from_slice(&0u16.to_le_bytes()); // packet_flags_extended
    h.extend_from_slice(&total.to_le_bytes());
    h.extend_from_slice(&9u16.to_le_bytes()); // radio_link_packet_index
    h.extend_from_slice(&src.to_le_bytes());
    h.extend_from_slice(&dest.to_le_bytes());
    h.extend(std::iter::repeat_n(0x77, extra));
    frame([0x08, 0x01], a1, RUBY_MAC, RUBY_MAC, &h)
}

/// A DroneBridge raw v2 frame: 10-byte header, optional 10-byte compatibility
/// padding, payload.
fn dronebridge(fc0: u8, dir: u8, comm: u8, port: u8, payload: usize, compat: bool) -> Vec<u8> {
    let len = (payload as u16).to_le_bytes();
    let mut f = vec![fc0, 0x00, 0x00, 0x00, dir, comm, port, len[0], len[1], 42];
    if compat {
        f.extend_from_slice(&[0x3C; 10]);
    }
    f.extend(std::iter::repeat_n(0x11, payload));
    f.extend_from_slice(&FCS);
    f
}

const PHONE: [u8; 6] = [0x5A, 0x1C, 0x9E, 0x03, 0x44, 0xB0]; // randomised: LA, unicast
const AP: [u8; 6] = [0x3C, 0x84, 0x6A, 0x11, 0x22, 0x33];

// ------------------------------------------------------------- positives --

#[test]
fn wfb_ng_default_link_video_downlink() {
    let got = classify(&wfb(0x08, WFB_DEFAULT_LINK_ID, 0, 1, 1400)).unwrap();
    assert_eq!(
        got,
        OpenLink {
            system: System::WfbNg,
            id: Some(0x7505D6),
            port: 0,
            direction: Direction::AirToGround,
            tx: [0x57, 0x42, 0x75, 0x05, 0xD6, 0x00],
        }
    );
    assert_eq!(got.wfb_stream(), Some(WfbStream::Video));
}

#[test]
fn wfb_ng_streams_directions_and_packet_types() {
    let cases = [
        (16, Direction::AirToGround, Some(WfbStream::Mavlink)),
        (32, Direction::AirToGround, Some(WfbStream::Tunnel)),
        (144, Direction::GroundToAir, Some(WfbStream::Mavlink)),
        (160, Direction::GroundToAir, Some(WfbStream::Tunnel)),
        (127, Direction::AirToGround, None),
        (255, Direction::GroundToAir, None),
    ];
    for (port, dir, stream) in cases {
        for ptype in [1, 2] {
            let l = classify(&wfb(0x08, 0x123456, port, ptype, 200)).unwrap();
            assert_eq!((l.port, l.direction, l.wfb_stream()), (port, dir, stream));
            assert_eq!(l.id, Some(0x123456));
        }
    }
}

#[test]
fn wfb_ng_rts_variant() {
    // `wfb_tx -T rts` puts the same header in an RTS frame.
    let l = classify(&wfb(0xB4, 0xABCDEF, 16, 1, 60)).unwrap();
    assert_eq!(
        (l.system, l.id, l.port),
        (System::WfbNg, Some(0xABCDEF), 16)
    );
}

#[test]
fn wfb_ng_tolerates_retry_and_power_bits() {
    let mut f = wfb(0x08, 1, 0, 1, 100);
    f[1] |= 0x08 | 0x10;
    assert!(classify(&f).is_some());
}

#[test]
fn openhd_air_and_ground() {
    // A random u32 start, then a few thousand packets later.
    let start = 0xF1E2_D3C4u64;
    let l = classify(&openhd(0x01, start + 5_000, 0x15)).unwrap();
    assert_eq!(
        l,
        OpenLink {
            system: System::OpenHd,
            id: None,
            port: 0x15,
            direction: Direction::AirToGround,
            tx: {
                let n = (start + 5_000).to_le_bytes();
                [0x01, n[0], n[1], n[2], n[3], 0x15]
            },
        }
    );
    let g = classify(&openhd(0x02, 7, 0xFE)).unwrap();
    assert_eq!((g.direction, g.port), (Direction::GroundToAir, 0xFE));
    // Past 2^32 packets the high half becomes 1: still OpenHD.
    assert!(classify(&openhd(0x01, (1 << 32) + 3, 4)).is_some());
}

#[test]
fn openhd_middle_bytes_change_per_packet() {
    let a = classify(&openhd(1, 100, 2)).unwrap();
    let b = classify(&openhd(1, 101, 2)).unwrap();
    assert_ne!(a.tx, b.tx);
    assert_eq!(
        (a.system, a.port, a.direction),
        (b.system, b.port, b.direction)
    );
}

#[test]
fn rubyfpv_downlink_and_uplink_ids() {
    let down = classify(&ruby(0x0F, 0x02, 0x1234_5678, 0x0BAD_F00D, 40)).unwrap();
    assert_eq!(
        down,
        OpenLink {
            system: System::RubyFpv,
            id: Some(0x1234_5678),
            port: 0x0F,
            direction: Direction::AirToGround,
            tx: RUBY_MAC,
        }
    );
    // Uplink: the controller is the source; the vehicle is the destination.
    let up = classify(&ruby(0x0E, 0x00, 0x0BAD_F00D, 0x1234_5678, 0)).unwrap();
    assert_eq!(
        (up.direction, up.id),
        (Direction::GroundToAir, Some(0x1234_5678))
    );
}

#[test]
fn rubyfpv_ids_withheld_when_not_clear() {
    // Encryption flag set: older versions encrypted the IDs too.
    let enc = classify(&ruby(0x0F, 1 << 6, 0x1234_5678, 1, 40)).unwrap();
    assert_eq!((enc.system, enc.id), (System::RubyFpv, None));
    // Uplink to the broadcast vehicle ID.
    let bc = classify(&ruby(0x0E, 0, 7, RUBY_BROADCAST_ID, 0)).unwrap();
    assert_eq!(bc.id, None);
    // A port we do not know: still Ruby, no direction, so no ID.
    let other = classify(&ruby(0x03, 0, 7, 8, 0)).unwrap();
    assert_eq!((other.direction, other.id), (Direction::Unknown, None));
    // total_length larger than the frame: an unknown layout, no ID.
    let mut f = ruby(0x0F, 0, 7, 8, 0);
    f[24 + 12] = 0xFF;
    assert_eq!(classify(&f).unwrap().id, None);
    // Body too short for a header: still Ruby by its addresses.
    let short = frame([0x08, 0x01], [0xFF; 6], RUBY_MAC, RUBY_MAC, &[1, 2, 3]);
    assert_eq!(classify(&short).unwrap().system, System::RubyFpv);
}

#[test]
fn dronebridge_frames() {
    let l = classify(&dronebridge(0x08, 0x03, 200, 3, 1000, false)).unwrap();
    assert_eq!(
        l,
        OpenLink {
            system: System::DroneBridge,
            id: Some(200),
            port: 3,
            direction: Direction::AirToGround,
            tx: [0; 6],
        }
    );
    let rc = classify(&dronebridge(0x08, 0x01, 200, 7, 14, false)).unwrap();
    assert_eq!((rc.direction, rc.port), (Direction::GroundToAir, 7));
    // RTS variant, and compatibility mode's 10 padding bytes.
    assert!(classify(&dronebridge(0xB4, 0x03, 1, 2, 6, false)).is_some());
    assert!(classify(&dronebridge(0x08, 0x03, 1, 2, 30, true)).is_some());
}

// ------------------------------------------------------------- negatives --

#[test]
fn ordinary_infrastructure_data_is_ignored() {
    // Station to AP (ToDS): A1 is the BSSID, never broadcast.
    let up = frame([0x08, 0x01], AP, PHONE, [0x10; 6], &[0xAA; 100]);
    assert_eq!(classify(&up), None);
    // AP to station (FromDS).
    let down = frame([0x08, 0x02], PHONE, AP, [0x10; 6], &[0xAA; 100]);
    assert_eq!(classify(&down), None);
    // QoS data (0x88), the common case on modern networks.
    let qos = frame([0x88, 0x01], AP, PHONE, [0x10; 6], &[0xAA; 100]);
    assert_eq!(classify(&qos), None);
    // Null data / QoS null.
    assert_eq!(classify(&frame([0x48, 0x11], AP, PHONE, AP, &[])), None);
    assert_eq!(classify(&frame([0xC8, 0x11], AP, PHONE, AP, &[])), None);
}

#[test]
fn broadcast_arp_is_ignored() {
    // LLC/SNAP + ARP request.
    let mut arp = vec![0xAA, 0xAA, 0x03, 0, 0, 0, 0x08, 0x06];
    arp.extend_from_slice(&[0, 1, 8, 0, 6, 4, 0, 1]);
    arp.extend_from_slice(&[0x11; 20]);
    // From the station: to the AP, broadcast only as the final destination.
    assert_eq!(
        classify(&frame([0x08, 0x01], AP, PHONE, [0xFF; 6], &arp)),
        None
    );
    // Repeated by the AP: FromDS with a broadcast receiver.
    assert_eq!(
        classify(&frame([0x08, 0x02], [0xFF; 6], AP, PHONE, &arp)),
        None
    );
    // Ad-hoc (no DS bits) broadcast ARP.
    assert_eq!(
        classify(&frame([0x08, 0x00], [0xFF; 6], PHONE, AP, &arp)),
        None
    );
}

#[test]
fn mesh_and_wds_four_address_frames_are_ignored() {
    // ToDS + FromDS. Even with wfb-ng-looking addresses and body.
    let mut f = wfb(0x08, 1, 0, 1, 100);
    f[1] = 0x03;
    assert_eq!(classify(&f), None);
    let mut o = openhd(1, 5, 2);
    o[1] = 0x03;
    assert_eq!(classify(&o), None);
    let mut r = ruby(0x0F, 0, 1, 2, 0);
    r[1] = 0x03;
    assert_eq!(classify(&r), None);
}

#[test]
fn frames_from_57_42_that_are_not_wfb_data_are_ignored() {
    let mac = [0x57, 0x42, 0x75, 0x05, 0xD6, 0x00];
    // Management frames carrying that address: beacon, probe request, action.
    for fc0 in [0x80, 0x40, 0xD0] {
        assert_eq!(
            classify(&frame([fc0, 0x00], [0xFF; 6], mac, mac, &[1; 60])),
            None
        );
    }
    // Control frames other than RTS (CTS, ACK, block ack).
    for fc0 in [0xC4, 0xD4, 0x94] {
        assert_eq!(
            classify(&frame([fc0, 0x01], [0xFF; 6], mac, mac, &[1; 60])),
            None
        );
    }
    // Data, but not the wfb-ng shape: A3 differs, unicast A1, FromDS.
    assert_eq!(
        classify(&frame([0x08, 0x01], [0xFF; 6], mac, AP, &[1; 60])),
        None
    );
    assert_eq!(classify(&frame([0x08, 0x01], AP, mac, mac, &[1; 60])), None);
    assert_eq!(
        classify(&frame([0x08, 0x02], [0xFF; 6], mac, mac, &[1; 60])),
        None
    );
    // Protected, or with HT Control (Order).
    let mut p = wfb(0x08, 1, 0, 1, 100);
    p[1] |= 0x40;
    assert_eq!(classify(&p), None);
    let mut h = wfb(0x08, 1, 0, 1, 100);
    h[1] |= 0x80;
    assert_eq!(classify(&h), None);
    // Packet types wfb-ng never puts on the air, and a body too short to be real.
    for t in [0, 3, 0x45, 0xFF] {
        assert_eq!(classify(&wfb(0x08, 1, 0, t, 100)), None, "type {t}");
    }
    assert_eq!(classify(&wfb(0x08, 1, 0, 1, 24)), None);
    assert!(classify(&wfb(0x08, 1, 0, 1, 25)).is_some());
}

#[test]
fn openhd_lookalikes_are_ignored() {
    // A station with a 02:.. randomised MAC on an ordinary network.
    let a2 = [0x02, 0x9F, 0x31, 0x77, 0xC0, 0x15];
    assert_eq!(classify(&frame([0x08, 0x01], AP, a2, AP, &[0; 64])), None);
    // Broadcast shape, but A3 is not OpenHD's: first byte, port or the nonce's
    // high bytes differ.
    let ok = openhd(2, 99, 0x15);
    for (at, v) in [(16, 0x01), (21, 0x16), (18, 0x01), (19, 0x01), (20, 0x01)] {
        let mut f = ok.clone();
        f[at] = v;
        assert_eq!(classify(&f), None, "byte {at} = {v:#x}");
    }
    // Unique IDs other than air (1) / ground (2).
    assert_eq!(classify(&openhd(3, 99, 1)), None);
    assert_eq!(classify(&openhd(0, 99, 1)), None);
    // Too short to hold the authentication tag.
    let n = 99u64.to_le_bytes();
    let short = frame(
        [0x08, 0x01],
        [0xFF; 6],
        [1, n[0], n[1], n[2], n[3], 4],
        [1, 0, 0, 0, 0, 4],
        &[0; 15],
    );
    assert_eq!(classify(&short), None);
}

#[test]
fn rubyfpv_lookalikes_are_ignored() {
    // Only one of A2/A3 is Ruby's, or A1's port nibble is even.
    let a1 = [0x0F | 0xF0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    assert_eq!(
        classify(&frame([0x08, 0x01], a1, RUBY_MAC, AP, &[0; 30])),
        None
    );
    assert_eq!(
        classify(&frame([0x08, 0x01], a1, AP, RUBY_MAC, &[0; 30])),
        None
    );
    let even = [0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    assert_eq!(
        classify(&frame([0x08, 0x01], even, RUBY_MAC, RUBY_MAC, &[0; 30])),
        None
    );
}

#[test]
fn dronebridge_lookalikes_are_ignored() {
    // Length off by one either way, and compat padding of the wrong size.
    let mut f = dronebridge(0x08, 3, 1, 2, 50, false);
    f.insert(20, 0);
    assert_eq!(classify(&f), None);
    let mut f = dronebridge(0x08, 3, 1, 2, 50, false);
    f.remove(20);
    assert_eq!(classify(&f), None);
    // Direction, port and duration out of range.
    assert_eq!(classify(&dronebridge(0x08, 2, 1, 2, 50, false)), None);
    assert_eq!(classify(&dronebridge(0x08, 3, 1, 0, 50, false)), None);
    assert_eq!(classify(&dronebridge(0x08, 3, 1, 8, 50, false)), None);
    let mut d = dronebridge(0x08, 3, 1, 2, 50, false);
    d[2] = 0x3A;
    assert_eq!(classify(&d), None);
    // Ad-hoc IPv4 multicast (01:00:5e..) and IPv6 (33:33..) with zero duration.
    let v4 = frame(
        [0x08, 0x00],
        [0x01, 0x00, 0x5E, 0x00, 0x00, 0xFB],
        PHONE,
        AP,
        &[0; 40],
    );
    assert_eq!(classify(&v4), None);
    let v6 = frame(
        [0x08, 0x00],
        [0x33, 0x33, 0x00, 0x00, 0x00, 0x01],
        PHONE,
        AP,
        &[0; 40],
    );
    assert_eq!(classify(&v6), None);
    // An ordinary RTS: 16 bytes, a real duration.
    let mut rts = vec![0xB4, 0x00, 0x2C, 0x01];
    rts.extend_from_slice(&AP);
    rts.extend_from_slice(&PHONE);
    rts.extend_from_slice(&FCS);
    assert_eq!(classify(&rts), None);
}

#[test]
fn short_and_empty_input() {
    assert_eq!(classify(&[]), None);
    assert_eq!(classify(&[0x08]), None);
    assert_eq!(classify(&FCS), None);
    // A wfb-ng frame cut at every length never panics and only matches whole.
    let f = wfb(0x08, 1, 0, 1, 40);
    for n in 0..f.len() {
        let _ = classify(&f[..n]);
        let _ = classify_mpdu(&f[..n]);
    }
}

/// Random frames, including ones forced into each system's frame shape, do
/// not match. This is the false-positive argument made concrete: without
/// the specific addresses, the shape alone is not enough.
#[test]
fn random_frames_do_not_match() {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut buf = [0u8; 300];
    let mut hits = 0;
    for i in 0..300_000 {
        for b in buf.iter_mut() {
            *b = next() as u8;
        }
        let len = 4 + (next() % 296) as usize;
        match i % 4 {
            // Fully random.
            0 => {}
            // Data, ToDS, broadcast A1: the wfb-ng / OpenHD shape.
            1 => {
                buf[..4].copy_from_slice(&[0x08, 0x01, 0, 0]);
                buf[4..10].fill(0xFF);
            }
            // The same, with a randomised-MAC-style A2 starting 02.
            2 => {
                buf[..4].copy_from_slice(&[0x08, 0x01, 0, 0]);
                buf[4..10].fill(0xFF);
                buf[10] = 0x02;
                buf[16] = 0x02;
            }
            // Ad-hoc data with zero duration and a DroneBridge direction byte.
            _ => {
                buf[..4].copy_from_slice(&[0x08, 0x00, 0, 0]);
                buf[4] = 0x03;
                buf[6] = 1 + (next() % 7) as u8;
            }
        }
        hits += usize::from(classify(&buf[..len]).is_some());
    }
    // DroneBridge's length check lets about 2 in 65,536 of the last group
    // through (its only random field); everything else must be zero.
    assert!(hits <= 8, "{hits} random frames matched");
}

#[test]
fn serde_names() {
    let l = classify(&wfb(0x08, 1, 0, 1, 100)).unwrap();
    let j = serde_json::to_value(l).unwrap();
    assert_eq!(j["system"], "WfbNg");
    assert_eq!(j["direction"], "AirToGround");
    assert_eq!(serde_json::to_value(System::OpenHd).unwrap(), "OpenHd");
}

#[test]
fn labels() {
    for s in System::ALL {
        assert!(!s.label().is_empty());
    }
    assert_eq!(System::OpenHd.id_name(), None);
    assert_eq!(System::WfbNg.id_name(), Some("link_id"));
}

// ------------------------------------------------------------- sightings --

fn link(port: u8) -> OpenLink {
    classify(&wfb(0x08, WFB_DEFAULT_LINK_ID, port, 1, 100)).unwrap()
}

#[test]
fn sightings_count_and_rate_limit() {
    let mut s: Sightings<4> = Sightings::new();
    let video = link(0);
    let mut reports = Vec::new();
    // 1000 frames over 2.5 s, checked every 250 ms.
    for i in 0..1000u64 {
        let t = i * 2_500;
        s.record(&video, 161, -60, t);
        if i % 100 == 99 {
            s.take_due(t, 1_000_000, |x| reports.push((t, x.packets)));
        }
    }
    // First report at once, then one per second.
    assert_eq!(
        reports,
        [(247_500, 100), (1_247_500, 500), (2_247_500, 900)]
    );
    // Nothing new: nothing due, however long we wait.
    s.take_due(9_000_000, 1_000_000, |x| reports.push((0, x.packets)));
    s.take_due(10_000_000, 1_000_000, |_| panic!("no new frames"));
    assert_eq!(reports.last(), Some(&(0, 1000)));
    let one = s.iter().next().unwrap();
    assert_eq!((one.channel, one.rssi, one.first_us), (161, -60, 0));
}

#[test]
fn sightings_report_a_full_table_in_turns() {
    // Injected wfb-ng frames with many link ids fill the table. A node with
    // room for 4 hub frames must not be handed 16 at once (the old
    // `take_due` did, every 250 ms); taken 4 at a time, every link is still
    // reported, the longest-waiting first.
    let mut s: Sightings<16> = Sightings::new();
    for id in 0..16u32 {
        let l = classify(&wfb(0x08, 0x10_0000 + id, 0, 1, 100)).unwrap();
        s.record(&l, 161, -60, 1);
    }
    let mut first = Vec::new();
    assert_eq!(s.take_due_max(10, 1_000_000, 4, |x| first.push(x.link.id)), 4);
    let mut rest = Vec::new();
    for _ in 0..3 {
        assert_eq!(s.take_due_max(10, 1_000_000, 4, |x| rest.push(x.link.id)), 4);
    }
    assert_eq!(s.take_due_max(10, 1_000_000, 4, |_| panic!("all reported")), 0);
    let mut all: Vec<_> = first.iter().chain(&rest).copied().collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 16);
    // A second later, new frames on every link: those reported first are
    // due first again, so nobody waits more than its turn.
    for id in 0..16u32 {
        let l = classify(&wfb(0x08, 0x10_0000 + id, 0, 1, 100)).unwrap();
        s.record(&l, 161, -60, 1_000_100);
    }
    let mut again = Vec::new();
    s.take_due_max(1_000_100, 1_000_000, 4, |x| again.push(x.link.id));
    assert_eq!(again, first);
    // Unbounded, the old call: everything due at once.
    let mut n = 0;
    s.take_due(1_000_100, 1_000_000, |_| n += 1);
    assert_eq!(n, 12);
}

#[test]
fn sightings_keep_streams_apart_and_evict_the_quietest() {
    let mut s: Sightings<2> = Sightings::new();
    s.record(&link(0), 161, -60, 0);
    s.record(&link(16), 161, -61, 10);
    s.record(&link(0), 161, -62, 20);
    assert_eq!(s.iter().count(), 2);
    // A third stream replaces port 16 (heard least recently).
    s.record(&link(144), 161, -70, 30);
    assert_eq!(s.evicted, 1);
    let mut ports: Vec<u8> = s.iter().map(|x| x.link.port).collect();
    ports.sort();
    assert_eq!(ports, [0, 144]);
    assert_eq!(s.iter().find(|x| x.link.port == 0).unwrap().packets, 2);
    // OpenHD's changing A2 is one link; its latest A2 is kept.
    let mut o: Sightings<2> = Sightings::new();
    let a = classify(&openhd(1, 10, 2)).unwrap();
    let b = classify(&openhd(1, 11, 2)).unwrap();
    o.record(&a, 149, -50, 0);
    o.record(&b, 149, -50, 1);
    assert_eq!(o.iter().count(), 1);
    assert_eq!(o.iter().next().unwrap().link.tx, b.tx);
    // A zero-sized table is harmless.
    let mut z: Sightings<0> = Sightings::new();
    z.record(&a, 1, 1, 1);
    assert_eq!(z.iter().count(), 0);
}

// ------------------------------------------------------- bench builders --

/// The test beacon's fake frames must classify as their system, with the
/// fake identifier, and never with a real default ID.
#[test]
fn fake_frames_classify() {
    use crate::build::*;
    for sys in System::ALL {
        for seq in [0u32, 1, 4095, 4096, 0x00FF_FFFF, u32::MAX] {
            let mut buf = [0u8; MAX_LEN + FCS.len()];
            let n = fake_frame(sys, seq, &mut buf).unwrap();
            buf[n..n + 4].copy_from_slice(&FCS);
            let got = classify(&buf[..n + 4]).unwrap_or_else(|| panic!("{sys:?} seq {seq}"));
            assert_eq!(got.system, sys);
            assert_eq!(got.id, fake_id(sys), "{sys:?}");
            assert_eq!(got.direction, Direction::AirToGround, "{sys:?}");
            assert_ne!(got.id, Some(WFB_DEFAULT_LINK_ID));
            // The MPDU alone also classifies (no FCS slot).
            assert_eq!(classify_mpdu(&buf[..n]), Some(got));
        }
    }
    // A buffer that is too small is refused, not truncated.
    assert_eq!(fake_frame(System::WfbNg, 0, &mut [0u8; 40]), None);
}

#[test]
fn fake_frames_carry_the_marker_text() {
    use crate::build::*;
    for sys in System::ALL {
        let mut buf = [0u8; MAX_LEN];
        let n = fake_frame(sys, 7, &mut buf).unwrap();
        let body = &buf[..n];
        assert!(
            body.windows(10).any(|w| w == &FAKE_PAYLOAD[..10]),
            "{sys:?} has no marker text"
        );
    }
}
