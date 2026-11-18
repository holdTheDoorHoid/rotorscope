//! 802.11 -> IPv4 -> UDP/TCP -> MAVLink extraction, the queue filter and
//! rate limit, and end to end: a QGC-01-style camera payload over open
//! Wi-Fi raises a `MavlinkAttack`, ordinary telemetry does not.

use std::vec::Vec;

use mavwatch::frame::encode::v2;
use mavwatch::msg;

use super::*;

const AP: [u8; 6] = [0x60, 0x01, 0x94, 0x11, 0x22, 0x33];
const AP2: [u8; 6] = [0x60, 0x01, 0x94, 0x44, 0x55, 0x66];
const DRONE: [u8; 6] = [0x5C, 0xCF, 0x7F, 0x01, 0x02, 0x03];
const PHONE: [u8; 6] = [0x02, 0x00, 0x00, 0xAA, 0xBB, 0xCC];
const EVIL: [u8; 6] = [0x02, 0xBA, 0xD0, 0x00, 0x00, 0x01];
const DRONE_IP: [u8; 4] = [192, 168, 4, 1];
const GCS_IP: [u8; 4] = [192, 168, 4, 2];

/// How to build one data frame.
#[derive(Clone, Copy)]
struct Opts {
    to_ds: bool,
    from_ds: bool,
    qos: bool,
    htc: bool,
    protected: bool,
    more_frag: bool,
    frag_no: u8,
    amsdu: bool,
    null: bool,
    ethertype: u16,
    ip_frag: u16,
    proto: u8,
    sport: u16,
    dport: u16,
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    /// Addresses 1-4 as they go in the header.
    a: [[u8; 6]; 4],
    fcs: bool,
}

impl Opts {
    /// The drone's access point sending to the ground station (FromDS).
    fn down() -> Self {
        Opts {
            to_ds: false,
            from_ds: true,
            qos: true,
            htc: false,
            protected: false,
            more_frag: false,
            frag_no: 0,
            amsdu: false,
            null: false,
            ethertype: 0x0800,
            ip_frag: 0x4000, // DF
            proto: 17,
            sport: 14555,
            dport: 14550,
            src_ip: DRONE_IP,
            dst_ip: GCS_IP,
            // A1 = DA (phone), A2 = BSSID, A3 = SA (the drone's own AP).
            a: [PHONE, AP, AP, [0; 6]],
            fcs: true,
        }
    }

    /// A station (phone, or anyone on the open network) to the drone (ToDS).
    fn up(from: [u8; 6], src_ip: [u8; 4]) -> Self {
        Opts { to_ds: true, from_ds: false, sport: 14550, dport: 14555, src_ip, dst_ip: DRONE_IP, a: [AP, from, AP, [0; 6]], ..Self::down() }
    }
}

fn frame(o: &Opts, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::new();
    let subtype: u8 = if o.null { 0x4 } else { 0 } | if o.qos { 0x8 } else { 0 };
    f.push(0x08 | (subtype << 4));
    let mut fc1 = 0u8;
    fc1 |= u8::from(o.to_ds) | u8::from(o.from_ds) << 1 | u8::from(o.more_frag) << 2;
    fc1 |= u8::from(o.protected) << 6 | u8::from(o.htc) << 7;
    f.push(fc1);
    f.extend([0, 0]); // duration
    for a in &o.a[..3] {
        f.extend(a);
    }
    f.extend([o.frag_no | 0x30, 0x12]); // sequence control
    if o.to_ds && o.from_ds {
        f.extend(o.a[3]);
    }
    if o.qos {
        f.extend([if o.amsdu { 0x80 } else { 0x00 }, 0]);
        if o.htc {
            f.extend([0xAA; 4]);
        }
    }
    f.extend([0xAA, 0xAA, 0x03, 0, 0, 0]);
    f.extend(o.ethertype.to_be_bytes());
    let l4_len = if o.proto == 6 { 20 } else { 8 } + payload.len();
    let total = (20 + l4_len) as u16;
    f.extend([0x45, 0]);
    f.extend(total.to_be_bytes());
    f.extend([0x12, 0x34]);
    f.extend(o.ip_frag.to_be_bytes());
    f.extend([64, o.proto, 0, 0]);
    f.extend(o.src_ip);
    f.extend(o.dst_ip);
    f.extend(o.sport.to_be_bytes());
    f.extend(o.dport.to_be_bytes());
    if o.proto == 6 {
        f.extend([0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x18, 0x10, 0, 0, 0, 0, 0]);
    } else {
        f.extend((l4_len as u16).to_be_bytes());
        f.extend([0, 0]);
    }
    f.extend(payload);
    if o.fcs {
        f.extend([0xDE, 0xAD, 0xBE, 0xEF]);
    }
    f
}

fn mav(seq: u8, sys: u8, comp: u8, msgid: u32, payload: &[u8]) -> Vec<u8> {
    let mut b = [0u8; 300];
    let n = v2(&mut b, seq, sys, comp, msgid, payload, None);
    b[..n].to_vec()
}

fn heartbeat(seq: u8, sys: u8, comp: u8) -> Vec<u8> {
    let mut p = [0u8; 9];
    p[4..].copy_from_slice(&[2, 3, 0x81, 4, 3]);
    mav(seq, sys, comp, msg::HEARTBEAT, &p)
}

fn camera_info(vendor: &[u8], model: &[u8], uri: &[u8]) -> [u8; 237] {
    use msg::camera_information as c;
    let mut p = [0u8; 237];
    p[c::VENDOR..c::VENDOR + vendor.len()].copy_from_slice(vendor);
    p[c::MODEL..c::MODEL + model.len()].copy_from_slice(model);
    p[c::URI..c::URI + uri.len()].copy_from_slice(uri);
    p[28] = 1;
    p
}

// ------------------------------------------------------------ parsing --

#[test]
fn qos_from_ds_udp() {
    let hb = heartbeat(1, 1, 1);
    let f = frame(&Opts::down(), &hb);
    let d = parse(&f[..f.len() - 4]).unwrap();
    assert_eq!(d.payload, &hb[..]);
    assert!(!d.truncated);
    assert_eq!(
        d.flow,
        Flow { bssid: AP, src_mac: AP, src_ip: DRONE_IP, dst_ip: GCS_IP, sport: 14555, dport: 14550, tcp: false }
    );
    // A trailing FCS is harmless: the lengths come from the IP header.
    assert_eq!(parse(&f).unwrap(), d);
}

#[test]
fn non_qos_and_ht_control() {
    let hb = heartbeat(1, 1, 1);
    let plain = Opts { qos: false, ..Opts::down() };
    assert_eq!(parse(&frame(&plain, &hb)).unwrap().payload, &hb[..]);
    // The Order bit on a non-QoS frame adds no HT Control field...
    let ordered = Opts { qos: false, htc: true, ..Opts::down() };
    let f = frame(&ordered, &hb);
    assert_eq!(f.len(), frame(&plain, &hb).len());
    assert_eq!(parse(&f).unwrap().payload, &hb[..]);
    // ...on a QoS frame it does (4 bytes).
    let htc = Opts { htc: true, ..Opts::down() };
    assert_eq!(parse(&frame(&htc, &hb)).unwrap().payload, &hb[..]);
}

#[test]
fn address_layouts() {
    let hb = heartbeat(1, 1, 1);
    // ToDS: A1 = BSSID, A2 = SA.
    let f = frame(&Opts::up(PHONE, GCS_IP), &hb);
    let d = parse(&f).unwrap();
    assert_eq!((d.flow.bssid, d.flow.src_mac), (AP, PHONE));
    // FromDS: A2 = BSSID, A3 = SA (a station behind the AP).
    let o = Opts { a: [PHONE, AP, DRONE, [0; 6]], ..Opts::down() };
    let f = frame(&o, &hb);
    let d = parse(&f).unwrap();
    assert_eq!((d.flow.bssid, d.flow.src_mac), (AP, DRONE));
    // Neither (ad hoc): A2 = SA, A3 = BSSID.
    let o = Opts { to_ds: false, from_ds: false, a: [PHONE, DRONE, AP2, [0; 6]], ..Opts::down() };
    let f = frame(&o, &hb);
    let d = parse(&f).unwrap();
    assert_eq!((d.flow.bssid, d.flow.src_mac), (AP2, DRONE));
    // Both (WDS / mesh): A2 = transmitter stands for the network, A4 = SA.
    let o = Opts { to_ds: true, from_ds: true, a: [AP2, AP, PHONE, DRONE], ..Opts::down() };
    let f = frame(&o, &hb);
    let d = parse(&f).unwrap();
    assert_eq!((d.flow.bssid, d.flow.src_mac), (AP, DRONE));
    assert_eq!(d.payload, &hb[..]);
    // Same with QoS + HT Control after the fourth address.
    let o = Opts { htc: true, ..o };
    assert_eq!(parse(&frame(&o, &hb)).unwrap().payload, &hb[..]);
}

#[test]
fn passed_over() {
    let hb = heartbeat(1, 1, 1);
    let d = Opts::down();
    let cases = [
        (Opts { protected: true, ..d }, Skip::Protected),
        (Opts { null: true, ..d }, Skip::NoBody),
        (Opts { more_frag: true, ..d }, Skip::Fragment),
        (Opts { frag_no: 1, ..d }, Skip::Fragment),
        (Opts { amsdu: true, ..d }, Skip::Amsdu),
        (Opts { ethertype: 0x0806, ..d }, Skip::NotIpv4),
        (Opts { ethertype: 0x86DD, ..d }, Skip::NotIpv4),
        (Opts { ip_frag: 0x2000, ..d }, Skip::IpFragment),
        (Opts { ip_frag: 0x0010, ..d }, Skip::IpFragment),
        (Opts { proto: 1, ..d }, Skip::Transport),
    ];
    for (o, want) in cases {
        assert_eq!(parse(&frame(&o, &hb)), Err(want));
        assert!(candidate(&frame(&o, &hb)).is_err());
    }
    // Management and control frames, and short junk.
    let mut beacon = frame(&d, &hb);
    beacon[0] = 0x80;
    assert_eq!(parse(&beacon), Err(Skip::NotData));
    assert_eq!(parse(&[0xD4, 0, 0, 0]), Err(Skip::NotData));
    let f = frame(&d, &hb);
    for n in 0..60 {
        assert!(parse(&f[..n]).is_err(), "{n}");
    }
}

#[test]
fn truncated_is_flagged() {
    let hb = heartbeat(1, 1, 1);
    let mut f = frame(&Opts { fcs: false, ..Opts::down() }, &hb);
    f.truncate(f.len() - 5);
    let d = parse(&f).unwrap();
    assert!(d.truncated);
    assert_eq!(d.payload, &hb[..hb.len() - 5]);
}

#[test]
fn tcp_segments() {
    let hb = heartbeat(1, 1, 1);
    let o = Opts { proto: 6, sport: 5760, dport: 40000, ..Opts::down() };
    let f = frame(&o, &hb);
    let d = candidate(&f).unwrap();
    assert!(d.flow.tcp);
    assert_eq!(d.payload, &hb[..]);
    // TCP only on a MAVLink port: HTTPS bytes, even one starting with a
    // plausible header, stay out.
    let o = Opts { proto: 6, sport: 443, dport: 40000, ..Opts::down() };
    assert_eq!(candidate(&frame(&o, &hb)), Err(Skip::NotMavlink));
}

#[test]
fn queue_filter() {
    let hb = heartbeat(1, 1, 1);
    // Any port, if the payload starts with a whole plausible frame.
    let o = Opts { sport: 40001, dport: 40002, ..Opts::down() };
    assert!(candidate(&frame(&o, &hb)).is_ok());
    // A frame's tail on a MAVLink port: kept (the stream joins it with
    // what came before), even without a start byte.
    let mut two = hb.clone();
    two.extend(heartbeat(2, 1, 1));
    assert!(candidate(&frame(&Opts::down(), &two[5..])).is_ok());
    assert!(candidate(&frame(&Opts::down(), &[0u8; 40])).is_ok());
    // The same on another port: not.
    assert_eq!(candidate(&frame(&o, &two[5..])), Err(Skip::NotMavlink));
    // DNS, NTP, a QUIC-like 0xFD byte with a nonsense header, empty.
    let dns = Opts { sport: 53, dport: 5353, ..Opts::down() };
    assert_eq!(candidate(&frame(&dns, &[0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0])), Err(Skip::NotMavlink));
    assert_eq!(candidate(&frame(&o, &[0xFD, 0xFF, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 1, 2])), Err(Skip::NotMavlink));
    assert_eq!(candidate(&frame(&o, &[0xFD, 9, 0x04, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])), Err(Skip::NotMavlink), "unknown incompat flag");
    assert_eq!(candidate(&frame(&Opts::down(), &[])), Err(Skip::NotMavlink));
    // MAVLink 1.
    let mut b = [0u8; 64];
    let n = mavwatch::frame::encode::v1(&mut b, 1, 1, 1, msg::HEARTBEAT, &[0u8; 9]);
    assert!(candidate(&frame(&o, &b[..n])).is_ok());
}

#[test]
fn bucket_rate() {
    let mut b = Bucket::new();
    // A burst at boot: BURST at once.
    let first: usize = (0..50).filter(|_| b.allow(0)).count();
    assert_eq!(first, BURST as usize);
    // 1000 offered a second for 10 s: about PER_S a second get through.
    let mut ok = 0;
    for i in 1..=10_000u64 {
        ok += usize::from(b.allow(i * 1_000));
    }
    assert!((990..=1010).contains(&ok), "{ok}");
    // A quiet minute refills only to BURST.
    let t = 70_000_000;
    assert_eq!((0..50).filter(|_| b.allow(t)).count(), BURST as usize);
    // Time going backwards never panics or mints tokens.
    assert!(!b.allow(0));
}

// --------------------------------------------------------- end to end --

/// Feed whole 802.11 frames through the node's path: callback filter, then
/// the task's analysis. Returns what would go to the hub.
fn run(frames: &[(Vec<u8>, u64)], channel: u8) -> Vec<(ThreatSighting, MavlinkEvidence)> {
    let mut w = MavWifi::new();
    let mut out = Vec::new();
    for (f, t_ms) in frames {
        let Ok(d) = candidate(f) else { continue };
        let m = Meta { flow: d.flow, channel, rssi: -48, up_us: t_ms * 1000, truncated: d.truncated };
        w.feed(&m, d.payload, &mut |s, e| out.push((s.clone(), e.clone())));
    }
    out
}

/// A drone on its own open network, talking to a phone: heartbeats,
/// position, status (and some message ids mavwatch does not know) down at
/// ~10 datagrams/s, a ground-station heartbeat up once a second, a normal
/// camera definition URI. One datagram may carry several frames.
fn benign(t0: u64, secs: u64, bssid: [u8; 6], sys: u8) -> Vec<(Vec<u8>, u64)> {
    let mut v = Vec::new();
    let (mut seq, mut gseq) = (17u8, 200u8);
    let down = Opts { a: [PHONE, bssid, bssid, [0; 6]], ..Opts::down() };
    let up = Opts { a: [bssid, PHONE, bssid, [0; 6]], ..Opts::up(PHONE, GCS_IP) };
    for i in 0..secs * 10 {
        let t = t0 + i * 100;
        let mut dg = Vec::new();
        for msgid in [msg::HEARTBEAT, msg::GLOBAL_POSITION_INT, msg::SYS_STATUS, msg::GPS_RAW_INT] {
            seq = seq.wrapping_add(1);
            let len = mavwatch::frame::info(msgid).unwrap().base_len as usize;
            dg.extend(mav(seq, sys, 1, msgid, &[1u8; 64][..len]));
            if i % 3 == 0 {
                // A message mavwatch cannot check (ATTITUDE, id 30).
                seq = seq.wrapping_add(1);
                let mut b = [0u8; 64];
                b[..4].copy_from_slice(&[0xFD, 28, 0, 0]);
                b[4] = seq;
                dg.extend(&b[..40]);
            }
        }
        v.push((frame(&down, &dg), t));
        if i % 10 == 0 {
            gseq = gseq.wrapping_add(1);
            v.push((frame(&up, &heartbeat(gseq, 255, 190)), t + 5));
        }
        if i == 30 {
            let cam = camera_info(b"Siyi", b"A8 mini", b"http://192.168.144.25/cam_def.xml");
            v.push((frame(&down, &mav(seq, sys, 100, msg::CAMERA_INFORMATION, &cam)), t + 7));
        }
    }
    v
}

#[test]
fn qgc01_camera_information_over_open_wifi() {
    let mut frames = benign(0, 5, AP, 1);
    let cam = camera_info(b"../../../../../tmp", b"QGC01", b"http://192.168.2.64:8000/c.xml");
    let evil = mav(9, 1, 100, msg::CAMERA_INFORMATION, &cam);
    frames.push((frame(&Opts::down(), &evil), 5_500));
    let got = run(&frames, 6);
    assert_eq!(got.len(), 1, "{got:?}");
    let (s, e) = &got[0];
    assert_eq!(s.kind, ThreatKind::MavlinkAttack);
    assert_eq!(s.detail, mavwatch::Kind::CameraName.code());
    assert_eq!((s.channel, s.freq, s.level), (6, 24_370, -48));
    assert_eq!(s.source, [0, 0, 0, 0, 1, 100]);
    assert_eq!(s.ssid.as_str(), "../../../../../tmp");
    assert_eq!(s.uptime_us, 5_500_000);
    assert_eq!((e.detail, e.sysid, e.compid, e.msgid), (s.detail, 1, 100, msg::CAMERA_INFORMATION));
    assert_eq!(&e.frame[..], &evil[..], "the raw MAVLink frame is kept");
    assert!(!e.truncated);
    // Both fit link frames.
    let mut buf = [0u8; link::MAX_FRAME];
    link::encode(&link::Frame::Threat(s.clone()), &mut buf).unwrap();
    link::encode(&link::Frame::MavlinkEvidence(e.clone()), &mut buf).unwrap();
}

#[test]
fn split_across_datagrams_and_tcp() {
    // The camera frame cut in two datagrams on a MAVLink port (a bridge
    // forwarding raw serial chunks) is joined by the flow's stream.
    let cam = camera_info(b"PX4", b"cam", b"mftp://../../root/m.plan");
    let evil = mav(3, 1, 100, msg::CAMERA_INFORMATION, &cam);
    let (a, b) = evil.split_at(100);
    let got = run(&[(frame(&Opts::down(), a), 1_000), (frame(&Opts::down(), b), 1_010)], 149);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0.detail, mavwatch::Kind::UriTraversal.code());
    assert_eq!(got[0].0.freq, 57_450);
    // Over TCP to 5760, in three segments.
    let o = Opts { proto: 6, sport: 5760, dport: 40000, ..Opts::down() };
    let segs: Vec<_> = evil.chunks(90).enumerate().map(|(i, c)| (frame(&o, c), 2_000 + i as u64)).collect();
    assert_eq!(run(&segs, 1).len(), 1);
    // Another flow's bytes in between do not spoil it (streams are per flow).
    let other = Opts { sport: 14556, ..Opts::down() };
    let mixed = [
        (frame(&Opts::down(), a), 1_000),
        (frame(&other, &evil[..60]), 1_005),
        (frame(&Opts::down(), b), 1_010),
    ];
    assert_eq!(run(&mixed, 6).len(), 1);
}

#[test]
fn benign_telemetry_over_wifi_is_quiet() {
    // A minute of one drone, then two drones on two networks with the same
    // system ID (1) and their own sequence counts, side by side.
    assert_eq!(run(&benign(0, 60, AP, 1), 6), []);
    let mut two = benign(0, 30, AP, 1);
    two.extend(benign(30, 30, AP2, 1));
    two.sort_by_key(|f| f.1);
    assert_eq!(run(&two, 6), []);
    // A protected copy of an exploit is unreadable and never looked at.
    let cam = camera_info(b"../../../../../tmp", b"QGC01", b"x");
    let wpa = Opts { protected: true, ..Opts::down() };
    assert_eq!(run(&[(frame(&wpa, &mav(9, 1, 100, msg::CAMERA_INFORMATION, &cam)), 0)], 6), []);
}

/// The link is the network: an injector on the drone's open network posing
/// as its ground station (255/190) with its own sequence count is caught
/// (`SeqInterleave`, kind 17); the same two counts on two networks are two
/// links and stay quiet.
#[test]
fn injector_on_the_same_network() {
    let real = Opts { a: [AP, PHONE, AP, [0; 6]], ..Opts::up(PHONE, GCS_IP) };
    let fake = Opts { a: [AP, EVIL, AP, [0; 6]], ..Opts::up(EVIL, [192, 168, 4, 66]) };
    let elsewhere = Opts { a: [AP2, EVIL, AP2, [0; 6]], ..fake };
    let script = |injector: &Opts| {
        let mut v = Vec::new();
        for i in 0..100u64 {
            let t = 1_000 + i * 100;
            v.push((frame(&real, &heartbeat(i as u8, 255, 190)), t));
            v.push((frame(injector, &heartbeat((i as u8).wrapping_add(128), 255, 190)), t + 50));
        }
        v
    };
    let got = run(&script(&fake), 6);
    assert!(!got.is_empty());
    assert!(got.iter().all(|(s, _)| s.detail == mavwatch::Kind::SeqInterleave.code()), "{got:?}");
    assert_eq!(got[0].0.source, [0, 0, 0, 0, 255, 190]);
    assert_eq!(run(&script(&elsewhere), 6), []);
}
