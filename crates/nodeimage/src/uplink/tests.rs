//! Host tests for the node's link glue against a real `airlink::v3::Hub`
//! (docs/LINK-KEYS-DESIGN.md section 9, node side). Fixed test keys only.

use std::string::String;
use std::vec::Vec;

use airlink::v3::{self, Hub, Reject};
use kitkeys::record::{self, NodeRecord, State};
use kitkeys::{KeySource, MasterKey, NodeIdent, NodeKey, SoftKeySource};
use link::{Assignment, Command, NodeHealth, NodeRole};

use super::*;

const ME: Mac = [0x24, 0x58, 0x7c, 0, 0, 0x0a];
const OTHER: Mac = [0x24, 0x58, 0x7c, 0, 0, 0x0b];
const GW: Mac = [0x24, 0x58, 0x7c, 0, 0, 0x99];

fn rng() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0x5000);
    N.fetch_add(1, Ordering::Relaxed)
}

fn kit() -> SoftKeySource {
    SoftKeySource::new(MasterKey::from_bytes([0x5a; 32]).unwrap())
}

fn ident(slot: u8, mac: Mac) -> NodeIdent {
    NodeIdent { slot, generation: 1, mac }
}

/// The partition `link-key node` would write for `mac` at `slot`.
fn partition(slot: u8, mac: Mac) -> record::PartitionImage {
    let rec = NodeRecord::provision(&mut kit(), ident(slot, mac)).unwrap();
    record::partition_image(&record::encode(&rec))
}

fn key(slot: u8, mac: Mac) -> NodeKey {
    kit().derive(&ident(slot, mac)).unwrap()
}

/// What the board reads at boot, on a board whose eFuse MAC is `efuse`.
fn state(p: &record::PartitionImage, efuse: Mac) -> State {
    record::read(&p.0, &efuse)
}

fn keyed_link() -> Link {
    Link::new(state(&partition(3, ME), ME), ME, Some(rng))
}

fn hub() -> Hub {
    let mut h = Hub::new(41);
    h.set_gateway(GW);
    assert!(h.set_slot(3, ME, &key(3, ME)));
    assert!(h.set_slot(4, OTHER, &key(4, OTHER)));
    h
}

fn line(l: &Link) -> String {
    let mut s = String::new();
    l.boot_line(&mut s).unwrap();
    s
}

fn wire(c: &Command) -> Vec<u8> {
    let mut b = [0u8; link::MAX_FRAME];
    link::encode_command(c, &mut b).unwrap().to_vec()
}

fn assign(id: u16) -> Command {
    Command::Assign(Assignment { role: NodeRole::Wifi24, plan: heapless::Vec::from_slice(&[(6, 100), (1, 100)]).unwrap(), id })
}

/// A hub, its gateway, and the node's link, with separate clocks.
struct Bench {
    hub: Hub,
    link: Link,
    ht: u64,
    nt: u64,
    seq: u16,
}

impl Bench {
    fn new() -> Self {
        Bench { hub: hub(), link: keyed_link(), ht: 500_000, nt: 3_000, seq: 0 }
    }

    fn seq(&mut self) -> u16 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn wait(&mut self, ms: u64) {
        self.ht += ms;
        self.nt += ms;
    }

    fn beacon(&mut self) -> Vec<u8> {
        let mut b = [0u8; v3::BEACON_MAX];
        let s = self.seq();
        let n = self.hub.seal_beacon(&mut b, s, 6, self.ht).unwrap();
        b[..n].to_vec()
    }

    fn command(&mut self, slot: u8, cmds: &[Command]) -> Vec<u8> {
        let frames: Vec<u8> = cmds.iter().flat_map(wire).collect();
        let mut b = [0u8; airlink::MAX_PACKET];
        let s = self.seq();
        let n = self.hub.seal_command(slot, &mut b, s, &frames, self.ht).unwrap();
        b[..n].to_vec()
    }

    fn hears<'a>(&mut self, p: &'a [u8]) -> Rx<'a> {
        self.link.receive(&GW, p, self.nt)
    }

    /// One attempt of a one-frame batch; what the hub made of it.
    fn uplink(&mut self, batch: &mut airlink::Batch) -> Option<Vec<u8>> {
        let s = self.seq();
        self.link.packet(batch, s, self.nt).map(<[u8]>::to_vec)
    }

    fn hub_takes(&mut self, p: &[u8]) -> bool {
        matches!(self.hub.open_frames(&ME, p, self.ht), v3::HubVerdict::Accept(_))
    }

    /// Beacon, batch, command: a proven link.
    fn link_up(&mut self) {
        let b = self.beacon();
        assert_eq!(self.hears(&b), Rx::Gateway);
        let mut batch = airlink::Batch::new();
        assert!(batch.push(&[1, 2, 0]));
        let up = self.uplink(&mut batch).unwrap();
        assert!(self.hub_takes(&up));
        let c = self.command(3, &[assign(9)]);
        assert!(matches!(self.hears(&c), Rx::Commands(_)));
        assert!(self.link.node().unwrap().proven());
    }
}

fn reject(l: &Link, r: Reject) -> u32 {
    l.counts.rejects.by_reason[r.index()]
}

// ----------------------------------------------------------------- states --

#[test]
fn keyed_boot_line_names_kit_slot_generation_and_check() {
    let p = partition(3, ME);
    let State::Keyed(rec) = state(&p, ME) else { panic!("keyed") };
    let want = std::format!(r#"{{"t":"uplink","auth":"{}","rng":"trng","link":"v3"}}"#, rec.auth_summary());
    let l = keyed_link();
    assert_eq!(line(&l), want);
    assert!(want.contains(" slot:3 gen:1 check:"), "{want}");
    assert!(want.starts_with(r#"{"t":"uplink","auth":"kit:"#), "the prefix tools/flash-node.sh looks for");
    assert_eq!(l.state_name(), "keyed");
    assert!(l.may_send());
    // The line never shows key bytes: kit id and check are public names.
    assert!(!want.contains("5a5a"));
}

#[test]
fn every_unprovisioned_state_blocks_reports() {
    let good = partition(3, ME);
    let mut damaged = record::PartitionImage(good.0);
    for o in record::COPY_OFFSETS {
        damaged.0[o + 20] ^= 1;
    }
    let cases: [(State, Option<fn() -> u32>, Why); 4] = [
        (record::read(&[0xFF; record::PARTITION_LEN], &ME), Some(rng), Why::Missing),
        (state(&damaged, ME), Some(rng), Why::Damaged),
        (state(&good, OTHER), Some(rng), Why::WrongBoard),
        (state(&good, ME), None, Why::NoRng),
    ];
    for (st, r, why) in cases {
        let mut l = Link::new(st, ME, r);
        assert_eq!(l.blocked(), Some(why));
        assert!(!l.may_send());
        let mut batch = airlink::Batch::new();
        assert!(batch.push(&[1, 0]));
        assert!(l.packet(&mut batch, 1, 10).is_none(), "{why:?}: no reports");
        let s = line(&l);
        assert!(s.starts_with(r#"{"t":"warn","what":"link_key","auth":"none","why":""#), "{s}");
        assert!(s.contains(std::format!(r#""why":"{}""#, why.name()).as_str()), "{s}");
        assert!(s.contains(r#""board_mac":"24:58:7c:00:00:0a""#), "{s}");
        // Nothing from the hub is taken, not even a genuine beacon.
        let mut b = Bench::new();
        let bc = b.beacon();
        assert_eq!(l.receive(&GW, &bc, 10), Rx::Nothing);
        assert!(l.take_new_gateway().is_none());
    }
    // ME's record on OTHER's board.
    let s = line(&Link::new(state(&good, OTHER), OTHER, Some(rng)));
    assert!(s.contains(r#""record_mac":"24:58:7c:00:00:0a","board_mac":"24:58:7c:00:00:0b""#), "{s}");
    let s = line(&Link::new(state(&good, ME), ME, None));
    assert!(s.contains(r#""rng":"none""#) && s.contains(r#""record":"kit:"#), "{s}");
}

#[test]
fn open_record_runs_the_open_link() {
    let p = record::partition_image(&record::encode_open());
    let mut l = Link::new(state(&p, ME), ME, Some(rng));
    let s = line(&l);
    assert!(s.contains(r#""auth":"open""#) && s.contains(r#""link":"v1""#), "{s}");
    // Version 1 beacon and commands, unauthenticated, as an open kit was.
    let mut buf = [0u8; 16];
    let bc = airlink::beacon(&mut buf, 1, 6).to_vec();
    assert_eq!(l.receive(&GW, &bc, 100), Rx::Gateway);
    assert_eq!(l.take_new_gateway(), Some((GW, 6)));
    let frames = wire(&assign(5));
    let mut cb = [0u8; 400];
    let cmd = airlink::command(&mut cb, 7, &frames).unwrap().to_vec();
    let Rx::Commands(p) = l.receive(&GW, &cmd, 110) else { panic!("command") };
    let mut got = Vec::new();
    commands(p, |c| got.push(c));
    assert_eq!(got, [assign(5)]);
    // A radio retry is a repeat, not a second command.
    assert_eq!(l.receive(&GW, &cmd, 120), Rx::Gateway);
    assert_eq!(l.counts.rejects.dups, 1);
    // Batches go out as version 1.
    let mut batch = airlink::Batch::new();
    assert!(batch.push(&[1, 2, 0]));
    let pkt = l.packet(&mut batch, 3, 130).unwrap().to_vec();
    assert_eq!(airlink::parse(&pkt).unwrap().sealed, false);
    // A version 2 (kit key) packet: old firmware. Version 3: not ours to read.
    let k = airlink::auth::Key::new(&[9; 32]);
    let mut v2 = [0u8; 128];
    let n = airlink::command(&mut v2, 1, &frames[..]).map(<[u8]>::len).unwrap();
    let st = airlink::auth::Stamp { epoch: 1, clock: 1, session: 1, counter: 1 };
    let n = airlink::auth::seal(&k, &GW, &mut v2, n, st).unwrap();
    assert_eq!(l.receive(&GW, &v2[..n], 140), Rx::Nothing);
    assert_eq!(reject(&l, Reject::OldFirmware), 1);
    let mut b = Bench::new();
    let v3b = b.beacon();
    assert_eq!(l.receive(&GW, &v3b, 150), Rx::Nothing);
}

// ------------------------------------------------------------ keyed link --

#[test]
fn keyed_link_beacon_batch_command() {
    let mut b = Bench::new();
    let mut batch = airlink::Batch::new();
    assert!(batch.push(&[1, 2, 0]));
    // No epoch yet: nothing can be sealed.
    assert!(b.uplink(&mut batch).is_none());
    let bc = b.beacon();
    assert_eq!(b.hears(&bc), Rx::Gateway);
    assert_eq!(b.link.take_new_gateway(), Some((GW, 6)));
    assert_eq!(b.link.take_new_gateway(), None);
    let up = b.uplink(&mut batch).unwrap();
    assert!(b.hub_takes(&up));
    b.link.sent(true, batch.frames(), b.nt);
    let c = b.command(3, &[assign(9), Command::To { node: ME }]);
    let Rx::Commands(p) = b.hears(&c) else { panic!("taken") };
    let mut got = Vec::new();
    commands(p, |c| got.push(c));
    assert_eq!(got, [assign(9), Command::To { node: ME }]);
    let c = b.link.counts;
    assert_eq!((c.heard, c.beacons, c.commands, c.batches_sent, c.frames_sent, c.send_attempts), (2, 1, 1, 1, 1, 1));
    // The same command again (a radio retry): a repeat.
    let again = b.command(3, &[assign(9)]);
    assert!(matches!(b.hears(&again), Rx::Commands(_)));
    assert_eq!(b.hears(&again), Rx::Gateway);
    assert_eq!(b.link.counts.rejects.dups, 1);
}

/// Every attempt is a new packet with a fresh stamp, so a batch retried for
/// its full 2 s is never late at the hub, and a lost attempt costs nothing.
#[test]
fn every_retry_is_sealed_again() {
    let mut b = Bench::new();
    let bc = b.beacon();
    b.hears(&bc);
    let mut batch = airlink::Batch::new();
    assert!(batch.push(&[7, 7, 0]));
    let first = b.uplink(&mut batch).unwrap();
    b.link.sent(false, batch.frames(), b.nt);
    b.wait(1_900);
    let second = b.uplink(&mut batch).unwrap();
    assert_ne!(first, second);
    assert!(b.hub_takes(&second), "a retry 1.9 s later is still fresh");
    b.link.sent(true, batch.frames(), b.nt);
    assert!(!b.hub_takes(&first), "the lost first attempt is now older than the accepted one");
    assert_eq!((b.link.counts.send_attempts, b.link.counts.batches_sent, b.link.counts.failed()), (2, 1, 1));
}

/// Section 9: a withheld batch is late at the hub (`forge hold 5`).
#[test]
fn withheld_batch_is_late_at_the_hub() {
    let mut b = Bench::new();
    let bc = b.beacon();
    b.hears(&bc);
    let mut batch = airlink::Batch::new();
    assert!(batch.push(&[7, 0]));
    let held = b.uplink(&mut batch).unwrap();
    b.wait(5_000);
    match b.hub.open_frames(&ME, &held, b.ht) {
        v3::HubVerdict::Reject { reason, .. } => assert_eq!(reason, Reject::LateUplink),
        v => panic!("{v:?}"),
    }
}

/// A Command holding a TimeSync gets the 0.5 s rule; a plain one sealed at
/// the same moment still gets 3 s.
#[test]
fn timesync_command_gets_the_strict_rule() {
    let mut b = Bench::new();
    b.link_up();
    let ts = b.command(3, &[Command::TimeSync { utc_ms: 1_700_000_000_000, pps: true }]);
    let plain = b.command(3, &[assign(10)]);
    assert!(has_timesync(v3::payload(&ts, &v3::inspect(&ts).unwrap())));
    assert!(!has_timesync(v3::payload(&plain, &v3::inspect(&plain).unwrap())));
    // Held back one second (nothing else heard meanwhile).
    b.nt += 1_000;
    assert_eq!(b.hears(&ts), Rx::Nothing);
    assert_eq!(reject(&b.link, Reject::Late), 1);
    assert!(matches!(b.hears(&plain), Rx::Commands(_)));
}

/// Section 9, captured node: a command sealed for another slot, a beacon
/// with another slot's tag, a forged tag; foreign and old packets counted.
#[test]
fn forged_and_foreign_packets_are_refused_and_counted() {
    let mut b = Bench::new();
    b.link_up();
    // A command for us with one bit flipped.
    let mut c = b.command(3, &[assign(11)]);
    let n = c.len();
    c[n - 1] ^= 1;
    assert_eq!(b.hears(&c), Rx::Nothing);
    assert_eq!(reject(&b.link, Reject::BadTag), 1);
    // A captured node (slot 4) seals a "command" with its own up key: it
    // cannot make a down tag for us.
    let mut node4 = v3::Node::new(v3::NodeKeys::new(4, OTHER, &key(4, OTHER)), rng);
    let bc = b.beacon();
    assert!(matches!(node4.accept(&GW, &bc, 1, false), v3::NodeVerdict::Accept(_)));
    let mut fake = airlink::Batch::new();
    assert!(fake.push(&wire(&assign(66))));
    let mut forged = fake.sealed_v3(1, &mut node4, 2).unwrap().to_vec();
    forged[3] = Kind::Command as u8;
    assert_eq!(b.link.receive(&OTHER, &forged, b.nt), Rx::Nothing);
    assert_eq!(reject(&b.link, Reject::BadTag), 2);
    // Another node's batch is not ours: not counted at all.
    let mut other = airlink::Batch::new();
    assert!(other.push(&[1, 0]));
    let theirs = other.sealed_v3(2, &mut node4, 3).unwrap().to_vec();
    let before = b.link.counts;
    assert_eq!(b.link.receive(&OTHER, &theirs, b.nt), Rx::Nothing);
    assert_eq!(b.link.counts, before);
    // Version 1 at a keyed node: unauthenticated; not ours at all: foreign.
    let mut buf = [0u8; 16];
    let v1 = airlink::beacon(&mut buf, 1, 6).to_vec();
    assert_eq!(b.hears(&v1), Rx::Nothing);
    assert_eq!(reject(&b.link, Reject::Unauthenticated), 1);
    assert_eq!(b.hears(b"hello world"), Rx::Nothing);
    assert_eq!(reject(&b.link, Reject::Foreign), 1);
    // Version 2 (an un-reflashed hub or gateway): old firmware.
    let k = airlink::auth::Key::new(&[9; 32]);
    let mut v2 = [0u8; 128];
    let n = airlink::command(&mut v2, 1, &[4, 1, 2, 3, 0]).map(<[u8]>::len).unwrap();
    let st = airlink::auth::Stamp { epoch: 1, clock: 1, session: 1, counter: 1 };
    let n = airlink::auth::seal(&k, &GW, &mut v2, n, st).unwrap();
    assert_eq!(b.hears(&v2[..n]), Rx::Nothing);
    assert_eq!(reject(&b.link, Reject::OldFirmware), 1);
    // None of it moved the gateway.
    assert_eq!(b.link.gateway(b.nt), Some((GW, 6)));
}

/// A hub that rebooted: its beacon is refused for the epoch (the node was
/// proven), but it is genuine, so the gateway is kept and the deaf watch
/// sees it as heard.
#[test]
fn genuine_packet_of_another_epoch_still_proves_the_gateway() {
    let mut b = Bench::new();
    b.link_up();
    let heard = b.link.counts.heard;
    let mut h2 = Hub::new(42);
    h2.set_gateway(GW);
    assert!(h2.set_slot(3, ME, &key(3, ME)));
    let mut buf = [0u8; v3::BEACON_MAX];
    let n = h2.seal_beacon(&mut buf, 1, 6, 1_000).unwrap();
    assert_eq!(b.hears(&buf[..n]), Rx::Gateway);
    assert_eq!(reject(&b.link, Reject::Unproven), 1);
    assert_eq!(b.link.counts.heard, heard + 1);
    assert_eq!(b.link.counts.beacons, 1, "taken beacons only");
}

#[test]
fn gateway_is_forgotten_unless_heard_or_reached() {
    let mut b = Bench::new();
    let bc = b.beacon();
    b.hears(&bc);
    let t = b.nt;
    assert!(b.link.gateway(t + GATEWAY_TIMEOUT_MS).is_some());
    b.link.sent(true, 1, t + GATEWAY_TIMEOUT_MS);
    assert!(b.link.gateway(t + 2 * GATEWAY_TIMEOUT_MS).is_some(), "an ACK proves it is there");
    b.link.sent(false, 1, t + 2 * GATEWAY_TIMEOUT_MS);
    assert!(b.link.gateway(t + 3 * GATEWAY_TIMEOUT_MS + 1).is_none());
}

// ---------------------------------------------------------- announcements --

#[test]
fn blocked_node_announces_every_ten_seconds() {
    let mut l = Link::new(State::Missing, ME, Some(rng));
    let mut buf = [0u8; v3::UNPROVISIONED_LEN];
    let (why, p) = l.announce(1_000, IMAGE_WIFI, "node-wifi", "0.1.0", &mut buf).unwrap();
    assert_eq!(why, Why::Missing);
    let a = v3::parse_unprovisioned(p).unwrap();
    assert_eq!((a.mac, a.image), (ME, IMAGE_WIFI));
    assert_eq!(&a.firmware[..15], b"node-wifi 0.1.0");
    assert_eq!(v3::inspect(p).unwrap().kind, Kind::Unprovisioned);
    assert!(l.announce(5_000, IMAGE_WIFI, "node-wifi", "0.1.0", &mut buf).is_none());
    assert!(l.announce(10_999, IMAGE_WIFI, "node-wifi", "0.1.0", &mut buf).is_none());
    assert!(l.announce(11_000, IMAGE_WIFI, "node-wifi", "0.1.0", &mut buf).is_some());
    assert_eq!(l.counts.announcements, 2);
    // No random source: the announcement says so.
    let mut l = Link::new(state(&partition(3, ME), ME), ME, None);
    let (why, p) = l.announce(0, IMAGE_SDR, "node-sdr", "0.1.0", &mut buf).unwrap();
    assert_eq!(why, Why::NoRng);
    assert_eq!(&v3::parse_unprovisioned(p).unwrap().firmware[..14], b"node-sdr norng");
    // A working node never announces.
    assert!(keyed_link().announce(0, IMAGE_WIFI, "node-wifi", "0.1.0", &mut buf).is_none());
}

// ------------------------------------------------------------------ health --

#[test]
fn node_health_carries_the_node_side_reasons_in_order() {
    let mut c = Counts::default();
    for (i, r) in Reject::ALL.iter().enumerate() {
        c.rejects.by_reason[r.index()] = 100 + i as u32;
    }
    c.rejects.dups = 7;
    c.frames_sent = 50;
    c.batches_sent = 10;
    c.batches_given_up = 2;
    c.send_attempts = 13;
    let mut h = NodeHealth::default();
    fill_health(&c, &mut h);
    for (i, &code) in link::NODE_REJECT_CODES.iter().enumerate() {
        let r = Reject::ALL.iter().find(|r| r.code() == code).unwrap();
        assert_eq!(h.rejects[i], c.rejects.by_reason[r.index()], "{}", r.name());
    }
    assert_eq!((h.frames_sent, h.batches_sent, h.batches_given_up, h.send_attempts, h.duplicates), (50, 10, 2, 13, 7));
    // The hub-only reasons have no place on a node.
    for r in [Reject::NotInRoster, Reject::LateUplink, Reject::FutureUplink] {
        assert!(NodeHealth::reject_index(r.code()).is_none());
    }
    let mut s = String::new();
    link_auth_line(&c, &mut s).unwrap();
    for n in ["bad_tag", "replay", "unauthenticated", "stale", "late", "old_firmware", "not_for_this_slot", "unproven", "malformed", "foreign", "dups"] {
        assert!(s.contains(&std::format!(r#""{n}":"#)), "{n}: {s}");
    }
    assert!(s.starts_with(r#"{"t":"link_auth","bad_tag":100"#) && s.ends_with(r#""dups":7}"#), "{s}");
}

#[test]
fn timesync_detection() {
    let ts = wire(&Command::TimeSync { utc_ms: 5, pps: false });
    let a = wire(&assign(1));
    assert!(has_timesync(&ts));
    assert!(!has_timesync(&a));
    let both: Vec<u8> = a.iter().chain(ts.iter()).copied().collect();
    assert!(has_timesync(&both));
    assert!(!has_timesync(&[]));
    assert!(!has_timesync(&[1, 2, 3]), "a truncated tail is not a command");
}
