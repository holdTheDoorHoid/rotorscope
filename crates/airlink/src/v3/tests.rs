//! Host tests for version 3 (docs/LINK-KEYS-DESIGN.md section 9, `airlink`
//! rows, and section 12 item 1). Fixed, obvious test keys only.

use std::vec::Vec;

use super::*;
use crate::Batch;
use crate::auth::Stamp;

const KA: [u8; 32] = [0x11; 32];
const KB: [u8; 32] = [0x22; 32];
const KC: [u8; 32] = [0x33; 32];
const MAC_A: Mac = [0x02, 0, 0, 0, 0, 0x0a];
const MAC_B: Mac = [0x02, 0, 0, 0, 0, 0x0b];
const MAC_C: Mac = [0x02, 0, 0, 0, 0, 0x0c];
const GW: Mac = [0x02, 0, 0, 0, 0, 0x99];
const CMD: &[u8] = &[4, 1, 2, 3, 0];

fn nk(b: [u8; 32]) -> NodeKey {
    NodeKey::from_bytes(b)
}

/// Deterministic "random" sessions for tests.
fn rng() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0x1000);
    N.fetch_add(1, Ordering::Relaxed)
}

fn hub_for(epoch: u32) -> Hub {
    let mut h = Hub::new(epoch);
    h.set_gateway(GW);
    assert!(h.set_slot(0, MAC_A, &nk(KA)));
    assert!(h.set_slot(1, MAC_B, &nk(KB)));
    h
}

fn node(slot: u8, mac: Mac, key: [u8; 32]) -> Node {
    Node::new(NodeKeys::new(slot, mac, &nk(key)), rng)
}

/// `(slot, (session, counter))` a hub would echo in a Sync.
type SyncAsk = (u8, (u32, u32));

/// A hub, its gateway and node A (slot 0), with separate uptimes.
struct Kit {
    hub: Hub,
    node: Node,
    /// Hub uptime, ms.
    ht: u64,
    /// Node uptime, ms.
    nt: u64,
    seq: u16,
}

impl Kit {
    fn new(epoch: u32) -> Self {
        Kit { hub: hub_for(epoch), node: node(0, MAC_A, KA), ht: 1_000_000, nt: 10_000, seq: 0 }
    }

    fn seq(&mut self) -> u16 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn wait(&mut self, ms: u64) {
        self.ht += ms;
        self.nt += ms;
    }

    fn reboot_hub(&mut self, epoch: u32) {
        self.hub = hub_for(epoch);
        self.ht = 2_000;
    }

    fn reboot_node(&mut self) {
        self.node = node(0, MAC_A, KA);
        self.nt = 1_500;
    }

    fn beacon(&mut self) -> Vec<u8> {
        let mut b = [0u8; BEACON_MAX];
        let seq = self.seq();
        let n = self.hub.seal_beacon(&mut b, seq, 6, self.ht).unwrap();
        b[..n].to_vec()
    }

    fn sync(&mut self, echo: (u32, u32)) -> Vec<u8> {
        let mut b = [0u8; SYNC_LEN];
        let seq = self.seq();
        let n = self.hub.seal_sync(0, echo, &mut b, seq, 6, self.ht).unwrap();
        b[..n].to_vec()
    }

    fn command(&mut self) -> Vec<u8> {
        let mut b = [0u8; 64];
        let seq = self.seq();
        let n = self.hub.seal_command(0, &mut b, seq, CMD, self.ht).unwrap();
        b[..n].to_vec()
    }

    fn hears(&mut self, p: &[u8]) -> Verdict {
        self.node.accept(&GW, p, self.nt, false).verdict()
    }

    fn hears_ts(&mut self, p: &[u8]) -> Verdict {
        self.node.accept(&GW, p, self.nt, true).verdict()
    }

    fn uplink(&mut self) -> Option<Vec<u8>> {
        let mut b = Batch::new();
        assert!(b.push(&[1, 2, 0]));
        let seq = self.seq();
        b.sealed_v3(seq, &mut self.node, self.nt).map(<[u8]>::to_vec)
    }

    fn hub_hears(&mut self, p: &[u8]) -> (Verdict, Option<SyncAsk>) {
        let v = self.hub.open_frames(&MAC_A, p, self.ht);
        (v.verdict(), v.wants_sync())
    }

    /// Beacon heard, batch accepted, command taken: a proven link.
    fn link(&mut self) {
        let bc = self.beacon();
        assert_eq!(self.hears(&bc), Verdict::Accept);
        let up = self.uplink().unwrap();
        assert_eq!(self.hub_hears(&up).0, Verdict::Accept);
        let c = self.command();
        assert_eq!(self.hears(&c), Verdict::Accept);
        assert!(self.node.proven());
    }

    /// `ms` of time with a beacon every 500 ms, heard or not (jammed, away).
    /// Returns the verdicts of those heard.
    fn run(&mut self, ms: u64, heard: bool) -> Vec<Verdict> {
        let mut out = Vec::new();
        let mut left = ms;
        while left >= 500 {
            self.wait(500);
            left -= 500;
            let bc = self.beacon();
            if heard {
                out.push(self.hears(&bc));
            }
        }
        self.wait(left);
        out
    }

    /// The node sends; if the hub asks for a Sync, it is sent and heard.
    fn resync(&mut self) -> Verdict {
        let up = self.uplink().expect("node can send");
        let (v, want) = self.hub_hears(&up);
        match want {
            Some((0, echo)) => {
                let s = self.sync(echo);
                self.hears(&s)
            }
            _ => v,
        }
    }
}

/// Seal a hub-to-node packet by hand with any key: what a forger holding
/// only that key could make.
fn forge_down(key: &LinkKey, kind: Kind, src: &Mac, payload: &[u8], stamp: Stamp) -> Vec<u8> {
    let mut b = [0u8; 128];
    header(&mut b, kind, 1);
    b[HEADER..HEADER + payload.len()].copy_from_slice(payload);
    let n = seal(key, src, &mut b, HEADER + payload.len(), &stamp).unwrap();
    b[..n].to_vec()
}

// ------------------------------------------------- design test plan rows --

#[test]
fn node_key_cannot_seal_for_another_mac() {
    let mut k = Kit::new(7);
    k.link();
    // Node A captured: it holds K_A only and claims B's MAC (it can set its
    // station MAC to anything).
    let mut forger = node(0, MAC_B, KA);
    let bc = k.beacon();
    assert_eq!(forger.accept(&GW, &bc, k.nt, false).verdict(), Verdict::Accept);
    let mut b = Batch::new();
    b.push(&[9, 9, 0]);
    let p = b.sealed_v3(1, &mut forger, k.nt).unwrap().to_vec();
    let v = k.hub.open_frames(&MAC_B, &p, k.ht);
    assert_eq!(v.verdict(), Verdict::Reject(Reject::BadTag));
    assert_eq!(v.wants_sync(), None, "a forgery never earns a Sync");
    // Nor does A's own genuine batch pass under B's address.
    let up = k.uplink().unwrap();
    assert_eq!(k.hub.open_frames(&MAC_B, &up, k.ht).verdict(), Verdict::Reject(Reject::BadTag));
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
    // B keeps reporting: its slot state did not move.
    let mut nb = node(1, MAC_B, KB);
    assert_eq!(nb.accept(&GW, &bc, k.nt, false).verdict(), Verdict::Accept);
    let mut b = Batch::new();
    b.push(&[7, 0]);
    let p = b.sealed_v3(2, &mut nb, k.nt).unwrap().to_vec();
    assert!(matches!(k.hub.open_frames(&MAC_B, &p, k.ht), HubVerdict::Accept(Uplink { slot: 1, .. })));
    // A captured node claiming B's slot does not even pass B's beacon tag.
    let mut as_b = node(1, MAC_A, KA);
    assert_eq!(as_b.accept(&GW, &bc, k.nt, false).verdict(), Verdict::Reject(Reject::BadTag));
}

#[test]
fn command_under_other_node_key_rejected() {
    let mut k = Kit::new(7);
    k.link();
    // B is a live node; A (captured) sniffs B's session from B's uplinks and
    // seals an Assign for B with the only down key it has, its own.
    let mut nb = node(1, MAC_B, KB);
    let bc = k.beacon();
    nb.accept(&GW, &bc, k.nt, false);
    let mut b = Batch::new();
    b.push(&[7, 0]);
    let up = b.sealed_v3(1, &mut nb, k.nt).unwrap().to_vec();
    let st = match k.hub.open_frames(&MAC_B, &up, k.ht) {
        HubVerdict::Accept(u) => u.stamp,
        v => panic!("{v:?}"),
    };
    let echo = Stamp { epoch: st.epoch, clock: st.clock + 1, session: st.session, counter: st.counter };
    let forged = forge_down(&nk(KA).down(), Kind::Command, &GW, CMD, echo);
    assert_eq!(nb.accept(&GW, &forged, k.nt, false).verdict(), Verdict::Reject(Reject::BadTag));
    // A genuine command made for A, delivered to B: also not B's.
    let ca = k.command();
    assert_eq!(nb.accept(&GW, &ca, k.nt, false).verdict(), Verdict::Reject(Reject::BadTag));
    // Its own up key does not help either (up and down differ).
    let forged = forge_down(&nk(KA).up(), Kind::Command, &GW, CMD, echo);
    assert_eq!(nb.accept(&GW, &forged, k.nt, false).verdict(), Verdict::Reject(Reject::BadTag));
    // The hub's genuine one is taken.
    let mut buf = [0u8; 64];
    let n = k.hub.seal_command(1, &mut buf, 3, CMD, k.ht).unwrap();
    assert_eq!(nb.accept(&GW, &buf[..n], k.nt, false).verdict(), Verdict::Accept);
}

/// Three nodes, one beacon: each checks only its own tag.
#[test]
fn beacon_tag_checked_per_slot() {
    let mut hub = hub_for(7);
    assert!(hub.set_slot(2, MAC_C, &nk(KC)));
    let mut buf = [0u8; BEACON_MAX];
    let n = hub.seal_beacon(&mut buf, 1, 6, 50_000).unwrap();
    assert_eq!(n, beacon_len(3));
    let bc = buf[..n].to_vec();
    let mut na = node(0, MAC_A, KA);
    let mut nb = node(1, MAC_B, KB);
    let mut nc = node(2, MAC_C, KC);
    for x in [&mut na, &mut nb, &mut nc] {
        let v = x.accept(&GW, &bc, 0, false);
        assert!(matches!(v, NodeVerdict::Accept(Down { kind: Kind::Beacon, channel: Some(6), .. })), "{v:?}");
    }
    // A damaged tag in B's slot fails B only.
    let mut t = bc.clone();
    t[BEACON_FIXED + 8] ^= 1;
    assert_eq!(nb.accept(&GW, &t, 0, false).verdict(), Verdict::Reject(Reject::BadTag));
    assert_eq!(na.accept(&GW, &t, 0, false).verdict(), Verdict::Accept);
    assert_eq!(nc.accept(&GW, &t, 0, false).verdict(), Verdict::Accept);
    // Damage to what every tag covers fails everyone.
    let mut t = bc.clone();
    t[HEADER] ^= 1; // channel
    for x in [&mut na, &mut nb, &mut nc] {
        assert_eq!(x.accept(&GW, &t, 0, false).verdict(), Verdict::Reject(Reject::BadTag));
    }
    // Under another gateway's address: no.
    assert_eq!(na.accept(&MAC_C, &bc, 0, false).verdict(), Verdict::Reject(Reject::BadTag));
    // Revoked slot 1: zero tag, B is told it is not in this beacon, the rest carry on.
    hub.clear_slot(1);
    let n = hub.seal_beacon(&mut buf, 2, 6, 51_000).unwrap();
    assert_eq!(buf[BEACON_FIXED + 8..BEACON_FIXED + 16], [0; 8]);
    assert_eq!(nb.accept(&GW, &buf[..n], 1000, false).verdict(), Verdict::Reject(Reject::NotForThisSlot));
    assert_eq!(na.accept(&GW, &buf[..n], 1000, false).verdict(), Verdict::Accept);
    assert_eq!(nc.accept(&GW, &buf[..n], 1000, false).verdict(), Verdict::Accept);
    // A node past the beacon's last slot.
    let mut far = node(9, [2, 0, 0, 0, 0, 0x19], [0x44; 32]);
    assert_eq!(far.accept(&GW, &buf[..n], 0, false).verdict(), Verdict::Reject(Reject::NotForThisSlot));
    // Slots 0 and 2 left; clearing 2 shortens the beacon.
    hub.clear_slot(2);
    assert_eq!(hub.seal_beacon(&mut buf, 3, 6, 52_000), Ok(beacon_len(1)));
}

#[test]
fn beacon_with_other_slot_tag_rejected() {
    let mut hub = hub_for(7);
    let mut buf = [0u8; BEACON_MAX];
    let n = hub.seal_beacon(&mut buf, 1, 6, 50_000).unwrap();
    let bc = buf[..n].to_vec();
    let mut nb = node(1, MAC_B, KB);
    // Captured A copies its own (genuine) tag into B's place.
    let mut t = bc.clone();
    t.copy_within(BEACON_FIXED..BEACON_FIXED + 8, BEACON_FIXED + 8);
    assert_eq!(nb.accept(&GW, &t, 0, false).verdict(), Verdict::Reject(Reject::BadTag));
    // A forges a whole beacon with a newer clock, tagging B's slot with A's key.
    let mut f = bc.clone();
    let st = Stamp { epoch: 7, clock: 9_999, session: 0, counter: 0 };
    st.write(&mut f[HEADER + 2..HEADER + 2 + STAMP_LEN]);
    let tag_a = beacon_tag(&nk(KA).down(), &GW, &f[..BEACON_FIXED]);
    f[BEACON_FIXED..BEACON_FIXED + 8].copy_from_slice(&tag_a);
    f[BEACON_FIXED + 8..BEACON_FIXED + 16].copy_from_slice(&tag_a);
    assert_eq!(nb.accept(&GW, &f, 0, false).verdict(), Verdict::Reject(Reject::BadTag));
    assert_eq!(nb.epoch(), None, "learned nothing");
    // The genuine one is fine.
    assert_eq!(nb.accept(&GW, &bc, 0, false).verdict(), Verdict::Accept);
}

/// A node batch held back is refused by the hub once 2 s old (fails on
/// version 2, where the gateway checks order only).
#[test]
fn withheld_batch_late_at_hub() {
    for (hold, want) in [(2_000, Verdict::Accept), (2_100, Verdict::Reject(Reject::LateUplink)), (5_000, Verdict::Reject(Reject::LateUplink))] {
        let mut k = Kit::new(7);
        k.link();
        k.wait(1_000);
        let bc = k.beacon();
        assert_eq!(k.hears(&bc), Verdict::Accept);
        let held = k.uplink().unwrap();
        k.run(hold, true);
        let (v, sync) = k.hub_hears(&held);
        assert_eq!(v, want, "held {hold} ms");
        if v != Verdict::Accept {
            assert!(sync.is_some(), "genuine: the hub may answer with a Sync");
            // Nothing from it was taken: the next fresh batch is fine.
            let fresh = k.uplink().unwrap();
            assert_eq!(k.hub_hears(&fresh).0, Verdict::Accept);
        }
    }
    // Held while newer batches got through: a replay (order before age).
    let mut k = Kit::new(7);
    k.link();
    let held = k.uplink().unwrap();
    k.wait(100);
    let newer = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&newer).0, Verdict::Accept);
    k.wait(5_000);
    assert_eq!(k.hub_hears(&held), (Verdict::Reject(Reject::Replay), None));
}

/// A command (or TimeSync) recorded, withheld for `delay_ms`, then released.
/// Before the recording the node was jammed for `jam_ms`; after it, the node
/// hears the hub's beacons (`heard_after`) or nothing.
fn withheld(jam_ms: u64, delay_ms: u64, heard_after: bool, timesync: bool) -> Verdict {
    let mut k = Kit::new(7);
    k.link();
    k.run(10_000, true);
    k.run(jam_ms, false);
    let rec = k.command();
    k.run(delay_ms, heard_after);
    if timesync { k.hears_ts(&rec) } else { k.hears(&rec) }
}

#[test]
fn command_late_after_3s() {
    // The node keeps hearing beacons (the attacker holds back only the command).
    assert_eq!(withheld(0, 2_900, true, false), Verdict::Accept);
    assert_eq!(withheld(0, 3_100, true, false), Verdict::Reject(Reject::Late));
    // The node hears nothing after the recording either: 300 ticks at 90 a second.
    assert_eq!(withheld(0, 3_300, false, false), Verdict::Accept);
    assert_eq!(withheld(0, 3_400, false, false), Verdict::Reject(Reject::Late));
    // A command the gateway retried for its full 2 s is never late.
    assert_eq!(withheld(0, 2_000, true, false), Verdict::Accept);
    assert_eq!(withheld(0, 2_000, false, false), Verdict::Accept);
}

#[test]
fn timesync_late_after_half_second() {
    assert_eq!(withheld(0, 450, true, true), Verdict::Accept);
    assert_eq!(withheld(0, 600, true, true), Verdict::Reject(Reject::Late));
    assert_eq!(withheld(0, 550, false, true), Verdict::Accept);
    assert_eq!(withheld(0, 600, false, true), Verdict::Reject(Reject::Late));
    // The gateway's 300 ms give-up for a TimeSync stays inside the slack.
    assert_eq!(withheld(0, 300, true, true), Verdict::Accept);
    // The same packet as an ordinary command is still fine at 600 ms.
    assert_eq!(withheld(0, 600, true, false), Verdict::Accept);
}

/// The longest hold accepted, ms, in 10 ms steps.
fn longest_hold(jam_ms: u64, timesync: bool) -> u64 {
    let mut d = 0;
    while withheld(jam_ms, d + 10, false, timesync) == Verdict::Accept {
        d += 10;
        assert!(d < 120_000);
    }
    d
}

/// Jammed for J seconds before the recording and until its release: the
/// bound grows by a ninth of a second (10 ticks / 90 a second) per jammed
/// second, not by J (version 2: about J x (rate - 1) + 16 s). Past the 60 s
/// relink the recording is dead outright.
#[test]
fn jammed_node_bound_is_tenth_of_jam() {
    let base = longest_hold(0, false);
    assert!((3_300..=3_350).contains(&base), "{base}");
    for j in [10u64, 30, 50] {
        let d = longest_hold(j * 1000, false);
        let extra = d - base;
        // 10 ticks per jammed second at the floor's 90 a second: 111 ms.
        assert!(extra.abs_diff(j * 1000 / 9) <= 20, "J={j}s: bound {d} ms");
        assert!(extra <= j * 112, "J={j}s: about a tenth of the jam");
    }
    let ts = longest_hold(0, true);
    assert!((550..=570).contains(&ts), "{ts}");
    assert!(longest_hold(30_000, true) - ts <= 30 * 112);
    // Jammed for more than a minute: the node starts a new session, the
    // recording echoes the old one.
    assert_eq!(withheld(70_000, 0, false, false), Verdict::Reject(Reject::Replay));
    assert_eq!(withheld(70_000, 0, false, true), Verdict::Reject(Reject::Replay));
}

#[test]
fn v2_packet_is_old_firmware() {
    use crate::auth;
    let key = auth::Key::new(&KA);
    let mut b = Batch::new();
    b.push(&[1, 0]);
    let st = Stamp { epoch: 7, clock: 1, session: 1, counter: 0 };
    let v2 = b.sealed(1, &key, &MAC_A, st).to_vec();
    assert_eq!(v2[2], auth::VERSION_AUTH);
    assert_eq!(inspect(&v2), Err(Reject::OldFirmware));
    let mut k = Kit::new(7);
    assert_eq!(k.hub_hears(&v2).0, Verdict::Reject(Reject::OldFirmware));
    let mut buf = [0u8; crate::BEACON_LEN + auth::TRAILER];
    let len = crate::beacon(&mut buf, 0, 6).len();
    let n = auth::seal(&key, &GW, &mut buf, len, st).unwrap();
    assert_eq!(k.hears(&buf[..n]), Verdict::Reject(Reject::OldFirmware));
    // An open-kit (version 1) packet is named as such.
    let open = b.packet(2).to_vec();
    assert_eq!(k.hub_hears(&open).0, Verdict::Reject(Reject::Unauthenticated));
    assert_eq!(k.hears(crate::beacon(&mut [0u8; 8], 0, 6)), Verdict::Reject(Reject::Unauthenticated));
    // And version 2 firmware does not parse a version 3 packet at all.
    k.link();
    let c = k.command();
    let bc = k.beacon();
    let up = k.uplink().unwrap();
    for p in [&c, &bc, &up] {
        assert!(crate::parse(p).is_none());
        assert!(auth::verify(&key, &GW, p).is_err());
    }
}

// ------------------------------------------------------------- epochs --

/// Rule 1: a node that has just booted takes a recorded beacon of an old
/// epoch, tentatively. That costs the uplinks until the hub's Sync; a
/// recorded TimeSync or command echoes the old session and is dead.
#[test]
fn just_booted_node_old_epoch_beacon_and_old_timesync() {
    let mut k = Kit::new(100);
    k.link();
    k.run(5_000, true);
    let old_beacon = k.beacon();
    let old_ts = k.command(); // sent alone, a TimeSync
    let old_cmd = k.command();
    assert_eq!(k.hears_ts(&old_ts), Verdict::Accept, "genuine when made");
    // Hub reboot (epoch 101), then the node reboots.
    k.reboot_hub(101);
    k.run(3_000, false);
    k.reboot_node();
    // The attacker is first: replays from the gateway's address.
    assert_eq!(k.hears(&old_beacon), Verdict::Accept);
    assert_eq!((k.node.epoch(), k.node.proven()), (Some(100), false));
    assert_eq!(k.hears_ts(&old_ts), Verdict::Reject(Reject::Replay));
    assert_eq!(k.hears(&old_cmd), Verdict::Reject(Reject::Replay));
    // The real beacon (higher epoch) moves a tentative node on at once.
    let bc = k.beacon();
    assert_eq!(k.hears(&bc), Verdict::Accept);
    assert_eq!(k.node.epoch(), Some(101));
    // Or, had the attacker kept it on 100: its batch is refused as stale and
    // the hub's Sync moves it. Replay the old beacon again first.
    k.reboot_node();
    assert_eq!(k.hears(&old_beacon), Verdict::Accept);
    let up = k.uplink().unwrap();
    let (v, want) = k.hub_hears(&up);
    assert_eq!(v, Verdict::Reject(Reject::Stale));
    let sync = k.sync(want.unwrap().1);
    assert_eq!(k.hears(&sync), Verdict::Accept);
    assert_eq!((k.node.epoch(), k.node.proven()), (Some(101), true));
    // From now on the recordings do nothing.
    assert_eq!(k.hears(&old_beacon), Verdict::Reject(Reject::Stale));
    assert_eq!(k.hears_ts(&old_ts), Verdict::Reject(Reject::Replay));
    let up = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
    let ts = k.command();
    assert_eq!(k.hears_ts(&ts), Verdict::Accept);
}

/// Same epoch, but an old beacon (an old clock) first after boot: the node's
/// batches are late at the hub until a real beacon or a Sync, then fine.
#[test]
fn just_booted_node_old_beacon_same_epoch() {
    let mut k = Kit::new(100);
    k.link();
    let old_beacon = k.beacon();
    k.run(30_000, true);
    k.reboot_node();
    assert_eq!(k.hears(&old_beacon), Verdict::Accept);
    let up = k.uplink().unwrap();
    let (v, want) = k.hub_hears(&up);
    assert_eq!(v, Verdict::Reject(Reject::LateUplink));
    let s = k.sync(want.unwrap().1);
    assert_eq!(k.hears(&s), Verdict::Accept);
    let up = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
    assert_eq!(k.hears(&old_beacon), Verdict::Reject(Reject::Late));
}

/// A node jammed for minutes relinks (rule 5): recordings for its old
/// session are dead, an old beacon is at most a tentative epoch, and it
/// recovers by itself.
#[test]
fn jammed_minutes_node_old_beacon_and_old_timesync() {
    let mut k = Kit::new(100);
    k.link();
    let old_epoch_beacon = k.beacon();
    k.reboot_hub(101);
    k.wait(1_000);
    assert_eq!(k.resync(), Verdict::Accept);
    let up = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
    let rec_beacon = k.beacon();
    let rec_ts = k.command();
    let rec_cmd = k.command();
    let session = k.node.session();
    // Under a minute of jam: the old epoch's beacon is stale, the TimeSync late.
    k.run(30_000, false);
    assert_eq!(k.hears(&old_epoch_beacon), Verdict::Reject(Reject::Stale));
    assert_eq!(k.hears_ts(&rec_ts), Verdict::Reject(Reject::Late));
    assert_eq!(k.node.relinks(), 0);
    // Five minutes in all.
    k.run(270_000, false);
    assert_eq!(k.hears_ts(&rec_ts), Verdict::Reject(Reject::Replay));
    assert_eq!(k.node.relinks(), 1);
    assert_ne!(k.node.session(), session);
    assert_eq!(k.hears(&rec_cmd), Verdict::Reject(Reject::Replay));
    // Like a node that has just booted, it takes a genuine old beacon tentatively.
    assert_eq!(k.hears(&old_epoch_beacon), Verdict::Accept);
    assert_eq!((k.node.epoch(), k.node.proven()), (Some(100), false));
    assert_eq!(k.hears(&rec_beacon), Verdict::Accept, "tentative: moves up to 101");
    assert_eq!(k.hears(&old_epoch_beacon), Verdict::Reject(Reject::Stale));
    // Its batch, stamped from a five-minute-old clock, is late; the Sync fixes it.
    assert_eq!(k.resync(), Verdict::Accept);
    assert!(k.node.proven());
    let up = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
    let c = k.command();
    assert_eq!(k.hears(&c), Verdict::Accept);
    assert_eq!(k.hears_ts(&rec_ts), Verdict::Reject(Reject::Replay));
    assert_eq!(k.hears(&rec_beacon), Verdict::Reject(Reject::Late));
}

/// A command and a TimeSync recorded before a node reboot are useless after
/// it, even with the hub in the same epoch and the clock in range.
#[test]
fn command_and_timesync_recorded_before_reboot_dead_after() {
    let mut k = Kit::new(7);
    k.link();
    let cmd = k.command();
    let ts = k.command();
    k.reboot_node();
    k.wait(200);
    let bc = k.beacon();
    assert_eq!(k.hears(&bc), Verdict::Accept);
    let up = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
    assert_eq!(k.hears(&cmd), Verdict::Reject(Reject::Replay));
    assert_eq!(k.hears_ts(&ts), Verdict::Reject(Reject::Replay));
    let fresh = k.command();
    assert_eq!(k.hears(&fresh), Verdict::Accept);
    assert_eq!(k.hears(&cmd), Verdict::Reject(Reject::Replay));
}

/// Rule 3: a hub reboot (higher epoch) moves a node only by a Sync. Beacons
/// and commands of another epoch do not move it; afterwards the dead boot's
/// packets are stale.
#[test]
fn hub_reboot_moves_node_by_sync_only() {
    let mut k = Kit::new(100);
    k.link();
    let old_cmd = k.command();
    let old_sync = {
        let e = k.hub.echo(0).unwrap();
        k.sync(e)
    };
    k.reboot_hub(101);
    let bc = k.beacon();
    let v = k.node.accept(&GW, &bc, k.nt, false);
    assert_eq!(v.verdict(), Verdict::Reject(Reject::Unproven));
    assert!(v.proves_gateway(), "a genuine beacon still shows a gateway is there");
    assert_eq!(k.node.epoch(), Some(100));
    // A command of the new epoch for this session (only a hub bug could make
    // one) does not move the node either.
    let st = Stamp { epoch: 101, clock: 1, session: k.node.session(), counter: 0 };
    let c = forge_down(&nk(KA).down(), Kind::Command, &GW, CMD, st);
    assert_eq!(k.hears(&c), Verdict::Reject(Reject::Unproven));
    let mut buf = [0u8; 64];
    assert_eq!(k.hub.seal_command(0, &mut buf, 1, CMD, k.ht), Err(SealError::NotHeard));
    assert_eq!(k.resync(), Verdict::Accept);
    assert_eq!(k.node.epoch(), Some(101));
    assert_eq!(k.hears(&old_cmd), Verdict::Reject(Reject::Stale));
    assert_eq!(k.hears(&old_sync), Verdict::Reject(Reject::Stale));
    let up = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
    let c = k.command();
    assert_eq!(k.hears(&c), Verdict::Accept);
}

/// Rule 6: a TimeSync is refused until an echoed packet has proven the epoch.
#[test]
fn timesync_needs_a_proven_epoch() {
    let mut k = Kit::new(7);
    let bc = k.beacon();
    assert_eq!(k.hears(&bc), Verdict::Accept);
    let up = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
    let ts = k.command();
    let v = k.node.accept(&GW, &ts, k.nt, true);
    assert_eq!(v.verdict(), Verdict::Reject(Reject::Unproven));
    assert!(v.proves_gateway());
    let c = k.command();
    assert_eq!(k.hears(&c), Verdict::Accept);
    let ts = k.command();
    assert_eq!(k.hears_ts(&ts), Verdict::Accept);
}

/// The hub is re-provisioned and its boot counter comes back LOWER than the
/// epoch its nodes follow. The node refuses everything of the lower epoch for
/// a minute, relinks, and follows the hub; no reflash, no power cycle. The
/// recordings from before stay dead.
#[test]
fn lower_counter_hub_recovers_without_reopening_replays() {
    let mut k = Kit::new(1_000);
    k.link();
    k.run(5_000, true);
    let hi_beacon = k.beacon();
    let hi_ts = k.command();
    let hi_cmd = k.command();
    let hi_sync = {
        let e = k.hub.echo(0).unwrap();
        k.sync(e)
    };
    k.reboot_hub(500); // same master and roster, new rsboot seed below 1000
    let start = k.nt;
    let mut syncs = 0;
    // The node keeps sending at epoch 1000; the hub answers with Syncs at 500.
    while k.node.relinks() == 0 {
        let bcs = k.run(1_000, true);
        assert!(bcs.iter().all(|v| matches!(v, Verdict::Reject(Reject::Stale) | Verdict::Accept)));
        if k.node.relinks() > 0 {
            break;
        }
        assert_eq!(k.node.epoch(), Some(1_000));
        if let Some(up) = k.uplink() {
            let (v, want) = k.hub_hears(&up);
            assert_eq!(v, Verdict::Reject(Reject::Stale));
            let s = k.sync(want.unwrap().1);
            assert_eq!(k.hears(&s), Verdict::Reject(Reject::Stale));
            syncs += 1;
        }
        assert!(k.nt - start < 70_000, "recovered within the relink time");
    }
    assert!(syncs > 50);
    assert!(k.nt - start >= RELINK_MS);
    // Relinked on the first beacon after the minute: tentatively on 500.
    assert_eq!((k.node.epoch(), k.node.proven()), (Some(500), false));
    let up = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
    let c = k.command();
    assert_eq!(k.hears(&c), Verdict::Accept);
    let ts = k.command();
    assert_eq!(k.hears_ts(&ts), Verdict::Accept);
    // Nothing recorded under the old provisioning works now.
    assert_eq!(k.hears(&hi_beacon), Verdict::Reject(Reject::Unproven));
    assert_eq!(k.node.epoch(), Some(500));
    assert_eq!(k.hears_ts(&hi_ts), Verdict::Reject(Reject::Replay));
    assert_eq!(k.hears(&hi_cmd), Verdict::Reject(Reject::Replay));
    assert_eq!(k.hears(&hi_sync), Verdict::Reject(Reject::Replay));
}

/// The attacker replays a beacon of the old, higher epoch to a node that has
/// just relinked onto the lower-counter hub: it is a tentative epoch until the
/// hub's Sync, which moves the node down (rule 4).
#[test]
fn lower_counter_hub_with_old_beacons_replayed() {
    let mut k = Kit::new(1_000);
    k.link();
    let hi_beacon = k.beacon();
    k.reboot_hub(500);
    k.reboot_node(); // as good as a relink
    let bc = k.beacon();
    assert_eq!(k.hears(&bc), Verdict::Accept);
    assert_eq!(k.hears(&hi_beacon), Verdict::Accept, "tentative: a higher epoch wins");
    assert_eq!(k.node.epoch(), Some(1_000));
    let bc = k.beacon();
    assert_eq!(k.hears(&bc), Verdict::Reject(Reject::Stale));
    assert_eq!(k.resync(), Verdict::Accept, "the Sync moves a tentative node down");
    assert_eq!((k.node.epoch(), k.node.proven()), (Some(500), true));
    assert_eq!(k.hears(&hi_beacon), Verdict::Reject(Reject::Unproven));
    let up = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
}

/// With a proven epoch, a Sync of a lower epoch is refused even though it
/// echoes this session (a dead hub boot's packet the gateway held back).
#[test]
fn proven_node_refuses_lower_epoch_sync() {
    let mut k = Kit::new(100);
    k.link();
    let up = k.uplink().unwrap();
    let (_, _) = k.hub_hears(&up);
    let held = {
        let e = k.hub.echo(0).unwrap();
        k.sync(e)
    };
    k.reboot_hub(101);
    assert_eq!(k.resync(), Verdict::Accept);
    assert_eq!(k.hears(&held), Verdict::Reject(Reject::Stale));
}

// --------------------------------------------------------- clock, order --

#[test]
fn hub_clock_is_ticks_and_strictly_increasing() {
    let mut h = Hub::new(1);
    assert_eq!(h.next_clock(0), Some(0));
    assert_eq!(h.next_clock(0), Some(1));
    assert_eq!(h.next_clock(5), Some(2));
    assert_eq!(h.next_clock(1_000), Some(100));
    assert_eq!(h.next_clock(1_000), Some(101));
    assert_eq!(h.now(1_000), 101);
    assert_eq!(h.now(2_000), 200);
    // Runs out (about 497 days) instead of wrapping.
    let days_497 = u64::from(u32::MAX) * TICK_MS;
    assert!(days_497 / 86_400_000 == 497);
    assert_eq!(h.next_clock(days_497), Some(u32::MAX));
    assert_eq!(h.next_clock(days_497), None);
    let mut h = Hub::new(1);
    assert_eq!(h.next_clock(days_497 + 10), None);
    h.set_gateway(GW);
    h.set_slot(0, MAC_A, &nk(KA));
    let mut buf = [0u8; BEACON_MAX];
    assert_eq!(h.seal_beacon(&mut buf, 0, 6, days_497 + 10), Err(SealError::ClockExhausted));
}

#[test]
fn node_floor_and_estimate() {
    let mut k = Kit::new(7);
    k.link();
    let c = k.hub.now(k.ht);
    let f0 = k.node.floor(k.nt).unwrap();
    assert!(f0 >= u64::from(c) - 1);
    k.wait(10_000);
    assert_eq!(k.node.floor(k.nt).unwrap(), f0 + 900, "90 ticks a second");
    let e = k.node.estimate(k.nt).unwrap();
    assert!(e.abs_diff(u64::from(k.hub.now(k.ht))) <= 2, "estimate tracks the hub clock");
    let st = k.node.next_up(k.nt).unwrap();
    assert_eq!(u64::from(st.clock), e);
    // Non-decreasing within the epoch even when a delayed packet arrives.
    let a = k.node.next_up(k.nt).unwrap();
    let b = k.node.next_up(k.nt).unwrap();
    assert!((a.clock, a.counter) < (b.clock, b.counter));
}

#[test]
fn batch_retried_for_2s_is_resealed_and_never_late() {
    let mut k = Kit::new(7);
    k.link();
    let mut b = Batch::new();
    b.push(&[5, 5, 0]);
    let first = b.sealed_v3(1, &mut k.node, k.nt).unwrap().to_vec();
    let mut last = first.clone();
    // The radio gives up 20 times; each attempt is re-sealed.
    for _ in 0..20 {
        k.wait(100);
        last = b.sealed_v3(1, &mut k.node, k.nt).unwrap().to_vec();
    }
    k.wait(500);
    assert_eq!(k.hub_hears(&last).0, Verdict::Accept);
    // The first attempt's bytes, 2.5 s on: older pair, a replay.
    assert_eq!(k.hub_hears(&first).0, Verdict::Reject(Reject::Replay));
    // Not re-sealed (a buggy retry) and nothing newer accepted: late.
    let mut k = Kit::new(7);
    k.link();
    let once = b.sealed_v3(1, &mut k.node, k.nt).unwrap().to_vec();
    k.wait(2_500);
    assert_eq!(k.hub_hears(&once).0, Verdict::Reject(Reject::LateUplink));
}

#[test]
fn future_uplink_refused() {
    let mut k = Kit::new(7);
    k.link();
    let now = k.hub.now(k.ht);
    let mk = |k: &mut Kit, clock: u32, counter: u32| {
        let st = Stamp { epoch: 7, clock, session: k.node.session(), counter };
        let mut buf = [0u8; 64];
        header(&mut buf, Kind::Frames, 1);
        buf[HEADER..HEADER + 2].copy_from_slice(&[1, 0]);
        let n = seal(&nk(KA).up(), &MAC_A, &mut buf, HEADER + 2, &st).unwrap();
        buf[..n].to_vec()
    };
    let p = mk(&mut k, now + 51, 1_000);
    let (v, sync) = k.hub_hears(&p);
    assert_eq!(v, Verdict::Reject(Reject::FutureUplink));
    assert!(sync.is_some());
    let p = mk(&mut k, now + 50, 1_000);
    assert_eq!(k.hub_hears(&p).0, Verdict::Accept);
}

#[test]
fn uplink_order_duplicates_and_no_state_change_before_the_tag() {
    let mut k = Kit::new(7);
    k.link();
    let p1 = k.uplink().unwrap();
    let p2 = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&p1).0, Verdict::Accept);
    assert_eq!(k.hub_hears(&p2).0, Verdict::Accept);
    assert_eq!(k.hub_hears(&p2).0, Verdict::Duplicate);
    assert_eq!(k.hub_hears(&p1).0, Verdict::Reject(Reject::Replay));
    // A forgery with a newer pair and a bad tag changes nothing.
    let echo = k.hub.echo(0);
    let mut f = k.uplink().unwrap();
    let keep = f.clone();
    let n = f.len();
    f[n - 1] ^= 1;
    assert_eq!(k.hub_hears(&f), (Verdict::Reject(Reject::BadTag), None));
    assert_eq!(k.hub.echo(0), echo);
    assert_eq!(k.hub_hears(&keep).0, Verdict::Accept);
    // Node side: the command retried by the gateway is a duplicate.
    let c = k.command();
    assert_eq!(k.hears(&c), Verdict::Accept);
    assert_eq!(k.hears(&c), Verdict::Duplicate);
    let mut counts = NodeRejects::default();
    assert!(!counts.take(Verdict::Duplicate));
    assert!(!counts.take(Verdict::Reject(Reject::Late)));
    assert_eq!((counts.dups, counts.by_reason[Reject::Late.index()]), (1, 1));
}

#[test]
fn hub_answers_only_genuine_stale_batches_with_a_sync() {
    let mut k = Kit::new(7);
    k.link();
    k.reboot_hub(8);
    let up = k.uplink().unwrap();
    let mut forged = up.clone();
    let n = forged.len();
    forged[n - 3] ^= 0x40;
    assert_eq!(k.hub_hears(&forged), (Verdict::Reject(Reject::BadTag), None));
    let (v, want) = k.hub_hears(&up);
    assert_eq!(v, Verdict::Reject(Reject::Stale));
    assert_eq!(want.map(|(slot, (session, _))| (slot, session)), Some((0, k.node.session())));
    // Not in the roster: refused before any work.
    assert_eq!(k.hub.open_frames(&MAC_C, &up, k.ht).verdict(), Verdict::Reject(Reject::NotInRoster));
}

#[test]
fn honest_traffic_is_never_late() {
    let mut k = Kit::new(7);
    k.link();
    // Ten minutes: beacons every 500 ms; batches every 200 ms reaching the hub
    // 30 ms later; a command every 3 s heard after the gateway's last retry
    // (1.9 s); a TimeSync every 10 s heard after 290 ms; every 5 s the node is
    // away from the gateway channel for its longest allowed 1 s, and every
    // 60 s the hub's beacon loop stalls for a second.
    for s in 0..600u64 {
        if s % 60 == 30 {
            k.wait(1_000);
        }
        for half in 0..2 {
            k.wait(500);
            let bc = k.beacon();
            let away = s % 5 == 0;
            if !away {
                assert_eq!(k.hears(&bc), Verdict::Accept, "s={s}");
            }
            if half == 0 && !away {
                let up = k.uplink().unwrap();
                k.wait(30);
                assert_eq!(k.hub_hears(&up).0, Verdict::Accept, "s={s}");
                k.wait(170);
                let up = k.uplink().unwrap();
                assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
            }
        }
        if s % 3 == 1 {
            let c = k.command();
            k.wait(1_900);
            assert_eq!(k.hears(&c), Verdict::Accept, "s={s}");
        }
        if s % 10 == 7 {
            let up = k.uplink().unwrap();
            assert_eq!(k.hub_hears(&up).0, Verdict::Accept);
            let ts = k.command();
            k.wait(290);
            assert_eq!(k.hears_ts(&ts), Verdict::Accept, "s={s}");
        }
    }
    assert_eq!(k.node.relinks(), 0);
}

#[test]
fn node_counter_wrap_starts_a_new_session() {
    let mut k = Kit::new(7);
    k.link();
    k.node_counter(u32::MAX - 1);
    let s1 = k.node.session();
    let p = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&p).0, Verdict::Accept);
    let last = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&last).0, Verdict::Accept);
    assert_ne!(k.node.session(), s1);
    assert!(k.uplink().is_none(), "waits for its estimate to move on");
    k.wait(20);
    let p = k.uplink().unwrap();
    assert_eq!(k.hub_hears(&p).0, Verdict::Accept, "(new clock, 0) is past (old clock, MAX)");
    assert_eq!(k.hub_hears(&last).0, Verdict::Reject(Reject::Replay));
}

impl Kit {
    /// Running the counter up honestly would take years.
    fn node_counter(&mut self, c: u32) {
        self.node.test_set_counter(c);
    }
}

// --------------------------------------------- wire, sizes, parsers --

#[test]
fn packet_sizes() {
    assert_eq!(SYNC_LEN, 35);
    assert_eq!(UNPROVISIONED_LEN, 29);
    assert_eq!(beacon_len(1), 32);
    assert_eq!(BEACON_MAX, 152);
    const { assert!(BEACON_MAX <= 160, "fits the gateway's 160-byte Beacon message") };
    assert_eq!(MAX_PAYLOAD, 990);
    let mut hub = Hub::new(1);
    hub.set_gateway(GW);
    for s in 0..SLOTS as u8 {
        assert!(hub.set_slot(s, [2, 0, 0, 0, 1, s], &nk([s + 1; 32])));
    }
    let mut buf = [0u8; MAX_PACKET + 8];
    assert_eq!(hub.seal_beacon(&mut buf, 0, 6, 0), Ok(BEACON_MAX));
    assert_eq!(hub.seal_beacon(&mut buf[..BEACON_MAX - 1], 0, 6, 0), Err(SealError::BufferTooSmall));
    assert!(!hub.set_slot(16, MAC_A, &nk(KA)));
    assert!(!hub.set_slot(3, [2, 0, 0, 0, 1, 4], &nk(KA)), "one MAC, one slot");
    // Full command: exactly MAX_PACKET.
    let mut n = node(0, [2, 0, 0, 0, 1, 0], [1; 32]);
    let bc = buf[..BEACON_MAX].to_vec();
    assert_eq!(n.accept(&GW, &bc, 0, false).verdict(), Verdict::Accept);
    let mut b = Batch::new();
    let mut f = [7u8; 100];
    f[99] = 0;
    while b.push(&f) {}
    while b.push(&[1, 0]) {}
    assert_eq!(b.payload_len(), MAX_PAYLOAD);
    let up = b.sealed_v3(1, &mut n, 0).unwrap().to_vec();
    assert_eq!(up.len(), MAX_PACKET);
    let v = hub.open_frames(&[2, 0, 0, 0, 1, 0], &up, 0);
    assert!(matches!(v, HubVerdict::Accept(Uplink { payload, .. }) if payload.len() == MAX_PAYLOAD), "{v:?}");
    let frames: Vec<u8> = core::iter::repeat_n(5u8, MAX_PAYLOAD - 1).chain([0]).collect();
    assert_eq!(hub.seal_command(0, &mut buf, 1, &frames, 0), Ok(MAX_PACKET));
    assert_eq!(n.accept(&GW, &buf[..MAX_PACKET], 0, false).verdict(), Verdict::Accept);
    let big: Vec<u8> = core::iter::repeat_n(5u8, MAX_PAYLOAD).chain([0]).collect();
    assert_eq!(hub.seal_command(0, &mut buf, 1, &big, 0), Err(SealError::TooBig));
    assert_eq!(hub.seal_command(0, &mut buf, 1, &[1, 2], 0), Err(SealError::NotWholeFrames));
    assert_eq!(hub.seal_command(0, &mut buf[..HEADER + CMD.len() + TRAILER - 1], 1, CMD, 0), Err(SealError::BufferTooSmall));
    // A node cannot seal past MAX_PACKET or an empty batch.
    let mut big = [0u8; MAX_PACKET + 64];
    assert_eq!(n.seal_frames(&mut big, MAX_PACKET - TRAILER + 1, 0, 0), None);
    assert_eq!(n.seal_frames(&mut big, HEADER, 0, 0), None);
    // Sync.
    assert_eq!(hub.seal_sync(0, (1, 1), &mut buf, 1, 6, 0), Ok(SYNC_LEN));
    assert_eq!(hub.seal_sync(0, (1, 1), &mut buf[..SYNC_LEN - 1], 1, 6, 0), Err(SealError::BufferTooSmall));
    assert_eq!(hub.seal_sync(16, (1, 1), &mut buf, 1, 6, 0), Err(SealError::NoSlot));
    // No gateway, no roster.
    let mut h = Hub::new(1);
    assert_eq!(h.seal_beacon(&mut buf, 0, 6, 0), Err(SealError::NoGateway));
    h.set_gateway(GW);
    assert_eq!(h.seal_beacon(&mut buf, 0, 6, 0), Err(SealError::NoSlots));
}

/// Every kind of packet, genuine, for the parser tests.
fn samples() -> Vec<Vec<u8>> {
    let mut k = Kit::new(7);
    k.link();
    let mut v = vec![k.beacon(), k.command(), k.uplink().unwrap()];
    let e = k.hub.echo(0).unwrap();
    v.push(k.sync(e));
    let mut buf = [0u8; 64];
    let a = Unprovisioned { mac: MAC_C, image: 2, firmware: *b"node-wifi 0.9\0\0\0" };
    v.push(unprovisioned(&mut buf, 3, &a).unwrap().to_vec());
    let mut hub = Hub::new(1);
    hub.set_gateway(GW);
    for s in 0..SLOTS as u8 {
        hub.set_slot(s, [2, 0, 0, 0, 1, s], &nk([s + 1; 32]));
    }
    let mut b = [0u8; BEACON_MAX];
    let n = hub.seal_beacon(&mut b, 0, 6, 0).unwrap();
    v.push(b[..n].to_vec());
    v
}

/// These parsers face the radio: no input may panic or read out of bounds,
/// and nothing truncated, extended or oversized is taken.
#[test]
fn truncated_oversized_and_junk_inputs_never_panic() {
    let mut na = node(0, MAC_A, KA);
    let mut hub = hub_for(7);
    let mut check = |d: &[u8], expect_reject: bool| {
        let _ = inspect(d);
        let _ = parse_unprovisioned(d);
        let _ = crate::parse(d);
        let a = na.accept(&GW, d, 1_000, false);
        let b = na.accept(&GW, d, 1_000, true);
        let h = hub.open_frames(&MAC_A, d, 1_000);
        if expect_reject {
            assert!(!matches!(a, NodeVerdict::Accept(_)), "{d:?}");
            assert!(!matches!(b, NodeVerdict::Accept(_)), "{d:?}");
            assert!(!matches!(h, HubVerdict::Accept(_)), "{d:?}");
        }
    };
    for p in samples() {
        for n in 0..p.len() {
            check(&p[..n], true);
            // Frames and Command have no fixed size: a cut one still has a
            // shape, and fails on its tag (checked above).
            let variable = matches!(inspect(&p), Ok(Shape { kind: Kind::Frames | Kind::Command, .. }));
            if n >= 3 && !variable {
                assert!(inspect(&p[..n]).is_err(), "truncated {n} of {}", p.len());
            }
        }
        for extra in 1..4 {
            let mut q = p.clone();
            q.extend(core::iter::repeat_n(0u8, extra));
            check(&q, true);
            assert!(inspect(&q).is_err() || matches!(inspect(&q), Ok(Shape { kind: Kind::Frames | Kind::Command, .. })));
        }
        for i in 0..p.len() {
            let mut q = p.clone();
            q[i] ^= 0xff;
            check(&q, false);
        }
    }
    // Oversized: refused by size before anything else.
    for len in [MAX_PACKET + 1, 1_470, 4_096] {
        let mut d = vec![0u8; len];
        d[..4].copy_from_slice(b"RS\x03\x02");
        assert_eq!(inspect(&d), Err(Reject::Malformed));
        check(&d, true);
    }
    // Junk with our magic and version, every kind, every short length, every n.
    let mut x: u32 = 0x1234_5678;
    for len in 0..200usize {
        for kind in 0..8u8 {
            let mut d: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            if len >= 4 {
                d[..4].copy_from_slice(&[b'R', b'S', 3, kind]);
            }
            check(&d, true);
        }
    }
    for n in 0..=255u8 {
        let mut d = vec![0u8; beacon_len(usize::from(n).min(17))];
        d[..4].copy_from_slice(b"RS\x03\x04");
        d[HEADER + 1] = n;
        check(&d, true);
    }
}

/// The gateway's API takes no key: it cannot tell a forgery from a genuine
/// packet, and has nothing to make a tag with.
#[test]
fn gateway_parse_api_takes_no_key_and_cannot_verify_or_forge() {
    // The whole gateway-facing surface, by signature: data in, no key.
    let _: fn(&[u8]) -> Result<Shape, Reject> = inspect;
    let _: fn(&[u8]) -> Option<Unprovisioned> = parse_unprovisioned;
    for p in samples() {
        let genuine = inspect(&p);
        let mut forged = p.clone();
        let n = forged.len();
        if genuine.is_ok_and(|s| s.kind != Kind::Unprovisioned) {
            for b in &mut forged[n - 8..] {
                *b ^= 0x5a;
            }
        }
        assert_eq!(inspect(&forged), genuine, "same shape: nothing was verified");
    }
    // What a gateway can do to a sealed packet (alter, re-address, re-number)
    // is caught at the ends.
    let mut k = Kit::new(7);
    k.link();
    let up = k.uplink().unwrap();
    for i in 0..up.len() {
        let mut q = up.clone();
        q[i] ^= 1;
        assert!(!matches!(k.hub.open_frames(&MAC_A, &q, k.ht), HubVerdict::Accept(_)), "byte {i}");
    }
    let c = k.command();
    for i in 0..c.len() {
        let mut q = c.clone();
        q[i] ^= 1;
        assert_ne!(k.hears(&q), Verdict::Accept, "byte {i}");
    }
}

#[test]
fn up_and_down_keys_differ_so_nothing_reflects() {
    let key = nk(KA);
    assert_ne!(key.up().mac(&[b"x"]), key.down().mac(&[b"x"]));
    let mut k = Kit::new(7);
    k.link();
    // The node's own batch, turned into a Command and sent back to it.
    let mut up = k.uplink().unwrap();
    up[3] = Kind::Command as u8;
    assert_eq!(k.node.accept(&MAC_A, &up, k.nt, false).verdict(), Verdict::Reject(Reject::BadTag));
    assert_eq!(k.hears(&up), Verdict::Reject(Reject::BadTag));
    // A hub command turned into a batch and fed back to the hub.
    let mut c = k.command();
    c[3] = Kind::Frames as u8;
    assert_eq!(k.hub_hears(&c).0, Verdict::Reject(Reject::BadTag));
    // Unchanged kinds in the wrong direction are refused outright.
    let up = k.uplink().unwrap();
    assert_eq!(k.hears(&up), Verdict::Reject(Reject::Malformed));
    let c = k.command();
    assert_eq!(k.hub_hears(&c).0, Verdict::Reject(Reject::Malformed));
}

#[test]
fn every_bit_flip_is_caught() {
    let mut k = Kit::new(7);
    k.link();
    let c = k.command();
    let e = k.hub.echo(0).unwrap();
    let s = k.sync(e);
    let bc = k.beacon();
    for p in [&c, &s] {
        for i in 0..p.len() {
            for bit in 0..8 {
                let mut q = p.clone();
                q[i] ^= 1 << bit;
                assert_ne!(k.hears(&q), Verdict::Accept, "byte {i} bit {bit}");
            }
        }
    }
    // A beacon: everything node A's tag covers, and A's tag itself (B's tag is not A's business).
    for i in (0..BEACON_FIXED + 8).filter(|&i| i < bc.len()) {
        for bit in 0..8 {
            let mut q = bc.clone();
            q[i] ^= 1 << bit;
            assert_ne!(k.hears(&q), Verdict::Accept, "byte {i} bit {bit}");
        }
    }
    let up = k.uplink().unwrap();
    for i in 0..up.len() {
        for bit in 0..8 {
            let mut q = up.clone();
            q[i] ^= 1 << bit;
            assert!(!matches!(k.hub.open_frames(&MAC_A, &q, k.ht), HubVerdict::Accept(_)), "byte {i} bit {bit}");
        }
    }
}

#[test]
fn unprovisioned_announcement_round_trip() {
    let a = Unprovisioned { mac: MAC_C, image: 1, firmware: *b"node-ble 0.4\0\0\0\0" };
    let mut buf = [0u8; 40];
    let p = unprovisioned(&mut buf, 9, &a).unwrap().to_vec();
    assert_eq!(p.len(), UNPROVISIONED_LEN);
    assert_eq!(inspect(&p).map(|s| (s.kind, s.seq)), Ok((Kind::Unprovisioned, 9)));
    assert_eq!(parse_unprovisioned(&p), Some(a));
    assert!(unprovisioned(&mut [0u8; 28], 0, &a).is_none());
    let mut k = Kit::new(7);
    assert_eq!(k.hears(&p), Verdict::Reject(Reject::Malformed));
    assert_eq!(k.hub_hears(&p).0, Verdict::Reject(Reject::Malformed));
}

#[test]
fn reject_codes_indexes_and_names() {
    use crate::auth::Reject as R2;
    for (i, r) in Reject::ALL.iter().enumerate() {
        assert_eq!(r.index(), i);
        for o in &Reject::ALL[..i] {
            assert_ne!(r.code(), o.code());
            assert_ne!(r.name(), o.name());
        }
        assert!(![5, 6, 10].contains(&r.code()), "taken by others");
    }
    // Codes shared with version 2 keep their numbers.
    for (a, b) in [(Reject::BadTag, R2::BadTag), (Reject::Replay, R2::Replay), (Reject::Unauthenticated, R2::Unauthenticated), (Reject::Stale, R2::Stale), (Reject::Late, R2::Late)] {
        assert_eq!(a.code(), b.code());
        assert_eq!(a.name(), b.name());
    }
    assert_eq!(
        [Reject::OldFirmware, Reject::NotInRoster, Reject::LateUplink, Reject::FutureUplink].map(Reject::code),
        [8, 9, 11, 12],
        "the design's ForgedLink details"
    );
}

/// Section 5's budget: about 18 SHA-256 blocks for a full packet.
#[test]
fn tag_cost_in_sha256_blocks() {
    assert_eq!(tag_blocks(MAX_PACKET - TAG_LEN), 18);
    assert_eq!(tag_blocks(SYNC_LEN - TAG_LEN), 2);
    assert_eq!(tag_blocks(BEACON_FIXED), 2, "per beacon tag");
    assert_eq!(tag_blocks(40 - TAG_LEN), 2);
}
