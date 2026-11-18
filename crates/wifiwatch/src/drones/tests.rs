//! Hijack sequences that must be reported, and everyday drone flying that
//! must not, through [`crate::Watch`] (all parts together, as a node runs
//! them).

use super::*;
use crate::Watch;
use crate::mac::build::*;
use crate::mac::{ASSOC_REQ, ASSOC_RESP, AUTH, DEAUTH, DISASSOC, REASSOC_REQ};
use std::vec::Vec;

/// A DJI Tello: DJI address, its own open network (wifidrone maker 1).
const TELLO: [u8; 6] = [0x60, 0x60, 0x1F, 0x5A, 0x1B, 0x2C];
const TELLO2: [u8; 6] = [0x60, 0x60, 0x1F, 0x77, 0x01, 0x02];
/// The pilot's phone (a per-network randomised address, as phones use).
const PILOT: [u8; 6] = [0x0A, 0x11, 0x22, 0x33, 0x44, 0x55];
const PILOT2: [u8; 6] = [0x0A, 0x11, 0x22, 0x33, 0x44, 0x66];
const FRIEND: [u8; 6] = [0x0E, 0x01, 0x02, 0x03, 0x04, 0x05];
/// The attacker's Wi-Fi adapter.
const ATTACKER: [u8; 6] = [0x00, 0xC0, 0xCA, 0x91, 0x22, 0x33];
const HOME_AP: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];

#[derive(Clone, Copy)]
struct Drone {
    bssid: [u8; 6],
    ssid: &'static [u8],
    /// Node time (ms) the drone's radio started.
    boot_ms: u64,
    ch: u8,
    privacy: bool,
}

const TELLO_NET: Drone = Drone { bssid: TELLO, ssid: b"TELLO-5A1B2C", boot_ms: 0, ch: 6, privacy: false };

/// A beacon with the drone's timestamp and DS channel.
fn drone_beacon(d: &Drone, now_ms: u64) -> Vec<u8> {
    let extra = if d.privacy {
        let mut e = rsn(2);
        e.extend(ie(3, &[d.ch]));
        e
    } else {
        ie(3, &[d.ch])
    };
    let mut f = beacon(d.bssid, d.ssid, d.privacy, &extra);
    let tsf = now_ms.saturating_sub(d.boot_ms) * 1000;
    f[24..32].copy_from_slice(&tsf.to_le_bytes());
    f
}

fn assoc_resp(sta: [u8; 6], bssid: [u8; 6], status: u16) -> Vec<u8> {
    let mut body = std::vec![0x01, 0x00];
    body.extend_from_slice(&status.to_le_bytes());
    body.extend_from_slice(&[0x01, 0xC0]);
    mgmt(ASSOC_RESP, sta, bssid, bssid, &body)
}

struct Sim {
    w: Watch,
    out: Vec<ThreatSighting>,
    t: u64,
}

impl Sim {
    /// Ten minutes after the drones in the tests started (unless a test
    /// restarts one), so their reason-7 deauths are not a fresh boot's.
    fn new() -> Self {
        Self { w: Watch::new(), out: Vec::new(), t: 600_000 }
    }

    fn mgmt(&mut self, f: &[u8]) {
        let out = &mut self.out;
        self.w.frame(f, -50, 6, self.t * 1000, &mut |s| out.push(s));
    }

    fn data(&mut self, f: &[u8]) {
        self.w.data(f, -50, 6, self.t * 1000);
    }

    fn tick(&mut self, ms: u64) {
        self.t += ms;
        let out = &mut self.out;
        self.w.poll(self.t * 1000, &mut |s| out.push(s));
    }

    /// `ms` of flying: a beacon from each drone every 100 ms, and each
    /// (pilot, drone) pair exchanging data every 50 ms.
    fn fly(&mut self, ms: u64, drones: &[Drone], pilots: &[([u8; 6], [u8; 6])]) {
        let end = self.t + ms;
        while self.t < end {
            if self.t.is_multiple_of(100) {
                for d in drones {
                    let b = drone_beacon(d, self.t);
                    self.mgmt(&b);
                }
            }
            for &(p, d) in pilots {
                self.data(&data(p, d, true));
                self.data(&data(p, d, false));
            }
            self.tick(50);
        }
    }

    /// A station joining the way a phone or `nmcli` does: authentication,
    /// association, the drone's acceptance.
    fn join(&mut self, sta: [u8; 6], bssid: [u8; 6]) {
        self.mgmt(&mgmt(AUTH, bssid, sta, bssid, &[0, 0, 1, 0, 0, 0]));
        self.mgmt(&mgmt(AUTH, sta, bssid, bssid, &[0, 0, 2, 0, 0, 0]));
        self.mgmt(&mgmt(ASSOC_REQ, bssid, sta, bssid, &[0x01, 0, 0x0A, 0]));
        self.mgmt(&assoc_resp(sta, bssid, 0));
    }

    fn of(&self, kind: ThreatKind) -> Vec<&ThreatSighting> {
        self.out.iter().filter(|s| s.kind == kind).collect()
    }

    fn drone_kinds(&self) -> Vec<ThreatKind> {
        self.out
            .iter()
            .map(|s| s.kind)
            .filter(|k| matches!(k, ThreatKind::DroneDeauth | ThreatKind::DroneTakeover | ThreatKind::DroneNetClone))
            .collect()
    }
}

/// aireplay-ng `-0 n -a <drone> -c <phone>`: per count, 64 deauths to the
/// phone "from" the drone and 64 to the drone "from" the phone, reason 7.
fn aireplay(sim: &mut Sim, n: usize, drone: [u8; 6], phone: [u8; 6]) {
    for _ in 0..n {
        for _ in 0..64 {
            sim.mgmt(&kill(DEAUTH, phone, drone, drone, 7));
            sim.mgmt(&kill(DEAUTH, drone, phone, drone, 7));
        }
        sim.tick(250);
    }
}

#[test]
fn skyjack_deauth_then_join() {
    let mut s = Sim::new();
    s.fly(5000, &[TELLO_NET], &[(PILOT, TELLO)]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
    // SkyJack: `aireplay-ng -0 3 -a <drone> -c <phone>`, then joins.
    aireplay(&mut s, 3, TELLO, PILOT);
    s.fly(500, &[TELLO_NET], &[]);
    s.join(ATTACKER, TELLO);
    s.fly(3000, &[TELLO_NET], &[(ATTACKER, TELLO)]);

    let d = s.of(ThreatKind::DroneDeauth);
    assert!(!d.is_empty(), "{:?}", s.out);
    let first = d[0];
    assert_eq!(first.target, TELLO);
    assert_eq!(first.ssid.as_str(), "TELLO-5A1B2C");
    assert_eq!(first.detail & 0xFFFF, 7);
    assert_ne!(first.detail & KILL_CONTROLLER, 0);
    assert_eq!(first.detail >> MAKER_SHIFT, 1, "DJI Tello");
    assert_eq!(wifidrone::maker_name((first.detail >> MAKER_SHIFT) as u8), Some("DJI Tello"));
    // The generic flood detector sees the same burst.
    assert!(!s.of(ThreatKind::Deauth).is_empty());

    let t = s.of(ThreatKind::DroneTakeover);
    assert!(!t.is_empty(), "{:?}", s.out);
    let t0 = t[0];
    assert_eq!((t0.source, t0.target), (ATTACKER, TELLO));
    assert_ne!(t0.detail & TAKE_AFTER_KICK, 0);
    assert_ne!(t0.detail & (TAKE_AUTH | TAKE_ASSOC | TAKE_ACCEPTED), 0);
    assert_eq!(t0.detail >> MAKER_SHIFT, 1);
    // Reported about once a second while the attacker flies, data seen.
    assert!((3..=5).contains(&t.len()), "{}", t.len());
    assert!(t.iter().any(|x| x.detail & TAKE_DATA != 0));
    // Every sighting fits a link frame.
    let mut buf = [0u8; link::MAX_FRAME];
    for x in &s.out {
        link::encode(&link::Frame::Threat(x.clone()), &mut buf).unwrap();
    }
}

#[test]
fn a_handful_of_deauths_is_enough() {
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[(PILOT, TELLO)]);
    // Three deauths heard over four seconds (a visited channel catches a
    // fraction of a burst): far below the generic flood bar.
    for _ in 0..3 {
        s.mgmt(&kill(DEAUTH, PILOT, TELLO, TELLO, 7));
        s.fly(1300, &[TELLO_NET], &[(PILOT, TELLO)]);
    }
    let d = s.of(ThreatKind::DroneDeauth);
    assert_eq!(d.len(), 1, "{:?}", s.out);
    assert_eq!(d[0].count, 3);
    assert!(s.of(ThreatKind::Deauth).is_empty());
    // The pilot came back (same address): not a takeover.
    assert!(s.of(ThreatKind::DroneTakeover).is_empty());
}

#[test]
fn retry_bit_deauths_with_new_numbers_count() {
    // An injector sets the Retry bit and advances the sequence number: the
    // phone acts on every one. Old rule: any retry was skipped.
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[(PILOT, TELLO)]);
    for i in 0..4u16 {
        s.mgmt(&with_seq(kill(DEAUTH, PILOT, TELLO, TELLO, 7), 200 + i, true));
        s.tick(100);
    }
    s.tick(300);
    let d = s.of(ThreatKind::DroneDeauth);
    assert!(!d.is_empty(), "{:?}", s.out);
    assert_eq!(d[0].count, 3, "reported at the third");
}

#[test]
fn drone_retransmissions_count_once() {
    // The drone kicking an idle phone: one deauth, retried six times (same
    // number) because the phone is gone and never acknowledges.
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[(PILOT, TELLO)]);
    s.mgmt(&with_seq(kill(DEAUTH, PILOT, TELLO, TELLO, 4), 77, false));
    for _ in 0..6 {
        s.mgmt(&with_seq(kill(DEAUTH, PILOT, TELLO, TELLO, 4), 77, true));
    }
    s.fly(6000, &[TELLO_NET], &[]);
    assert!(s.of(ThreatKind::DroneDeauth).is_empty(), "{:?}", s.out);
}

#[test]
fn deauth_to_broadcast_and_disassoc_count() {
    let mut s = Sim::new();
    s.fly(2000, &[TELLO_NET], &[(PILOT, TELLO)]);
    s.mgmt(&kill(DEAUTH, BROADCAST, TELLO, TELLO, 3));
    s.mgmt(&kill(DISASSOC, BROADCAST, TELLO, TELLO, 8));
    s.mgmt(&kill(DISASSOC, PILOT, TELLO, TELLO, 8));
    s.tick(300);
    let d = s.of(ThreatKind::DroneDeauth);
    assert_eq!(d.len(), 1);
    assert_ne!(d[0].detail & floods::DETAIL_BROADCAST, 0);
    assert_ne!(d[0].detail & KILL_DISASSOC, 0);
}

#[test]
fn dronejack_joins_while_the_pilot_flies() {
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[(PILOT, TELLO)]);
    // DroneJack joins first, then deauths the others.
    s.join(ATTACKER, TELLO);
    s.fly(300, &[TELLO_NET], &[(PILOT, TELLO), (ATTACKER, TELLO)]);
    let t = s.of(ThreatKind::DroneTakeover);
    assert!(!t.is_empty(), "{:?}", s.out);
    assert_eq!(t[0].source, ATTACKER);
    assert_ne!(t[0].detail & TAKE_TWO_CONTROLLERS, 0);
    assert_eq!(t[0].detail & TAKE_AFTER_KICK, 0);
}

#[test]
fn one_frame_kick_races_the_pilot() {
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[(PILOT, TELLO)]);
    s.mgmt(&kill(DEAUTH, PILOT, TELLO, TELLO, 7));
    s.tick(500);
    s.join(ATTACKER, TELLO);
    s.tick(500);
    let t = s.of(ThreatKind::DroneTakeover);
    assert_eq!(t.len(), 1, "{:?}", s.out);
    assert_ne!(t[0].detail & TAKE_AFTER_LEAVE, 0);
    assert!(s.of(ThreatKind::DroneDeauth).is_empty(), "one frame is not a deauth attack");
}

#[test]
fn no_data_frames_still_sees_a_takeover() {
    // A node with data frames off learns the pilot from its joining only.
    let mut s = Sim::new();
    s.fly(1000, &[TELLO_NET], &[]);
    s.join(PILOT, TELLO);
    s.fly(4000, &[TELLO_NET], &[]);
    aireplay(&mut s, 1, TELLO, PILOT);
    s.fly(1000, &[TELLO_NET], &[]);
    s.join(ATTACKER, TELLO);
    s.tick(300);
    let t = s.of(ThreatKind::DroneTakeover);
    assert_eq!(t.len(), 1, "{:?}", s.out);
    assert_eq!(t[0].source, ATTACKER);
}

#[test]
fn a_single_stray_frame_is_not_a_takeover() {
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[(PILOT, TELLO)]);
    // One authentication request from some other station.
    s.mgmt(&mgmt(AUTH, TELLO, FRIEND, TELLO, &[0, 0, 1, 0, 0, 0]));
    s.fly(12_000, &[TELLO_NET], &[(PILOT, TELLO)]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn pilot_reconnects_after_the_drone_restarts() {
    let mut s = Sim::new();
    s.fly(5000, &[TELLO_NET], &[(PILOT, TELLO)]);
    // Battery swap: no beacons for 15 s; the phone keeps trying.
    for _ in 0..30 {
        s.data(&data(PILOT, TELLO, true));
        s.tick(500);
    }
    let rebooted = Drone { boot_ms: s.t, ..TELLO_NET };
    s.fly(300, &[rebooted], &[]);
    // The new drone does not know the phone: reason 7 to each of its frames.
    for _ in 0..6 {
        s.data(&data(PILOT, TELLO, true));
        s.mgmt(&kill(DEAUTH, PILOT, TELLO, TELLO, 7));
        s.tick(100);
    }
    s.join(PILOT, TELLO);
    s.fly(5000, &[rebooted], &[(PILOT, TELLO)]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);

    // Same again, but the phone forgot the network and comes back with a
    // new random address.
    let mut s = Sim::new();
    s.fly(5000, &[TELLO_NET], &[(PILOT, TELLO)]);
    s.tick(15_000);
    let rebooted = Drone { boot_ms: s.t, ..TELLO_NET };
    s.fly(2000, &[rebooted], &[]);
    s.join(PILOT2, TELLO);
    s.fly(5000, &[rebooted], &[(PILOT2, TELLO)]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn reason_7_from_an_old_drone_does_count() {
    // The same reason-7 burst from a drone that has been up for minutes is
    // what aireplay-ng sends.
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[(PILOT, TELLO)]);
    for _ in 0..4 {
        s.mgmt(&kill(DEAUTH, PILOT, TELLO, TELLO, 7));
        s.tick(100);
    }
    s.tick(300);
    assert_eq!(s.of(ThreatKind::DroneDeauth).len(), 1, "{:?}", s.out);
}

#[test]
fn phone_roaming_away_and_back() {
    let mut s = Sim::new();
    s.fly(5000, &[TELLO_NET], &[(PILOT, TELLO)]);
    // The phone leaves for the home network: a disassoc and a deauth (and
    // their retries).
    s.mgmt(&kill(DISASSOC, TELLO, PILOT, TELLO, 8));
    let mut r = kill(DEAUTH, TELLO, PILOT, TELLO, 3);
    s.mgmt(&r);
    r[1] |= 0x08;
    for _ in 0..5 {
        s.mgmt(&r);
    }
    s.mgmt(&mgmt(REASSOC_REQ, HOME_AP, PILOT, HOME_AP, &[0x01, 0, 0x0A, 0]));
    s.fly(20_000, &[TELLO_NET], &[]);
    // ...and back.
    s.mgmt(&mgmt(REASSOC_REQ, TELLO, PILOT, TELLO, &[0x01, 0, 0x0A, 0]));
    s.mgmt(&assoc_resp(PILOT, TELLO, 0));
    s.fly(5000, &[TELLO_NET], &[(PILOT, TELLO)]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn handing_the_drone_to_a_friend() {
    let mut s = Sim::new();
    s.fly(5000, &[TELLO_NET], &[(PILOT, TELLO)]);
    s.mgmt(&kill(DEAUTH, TELLO, PILOT, TELLO, 3));
    s.fly(15_000, &[TELLO_NET], &[]);
    s.join(FRIEND, TELLO);
    s.fly(5000, &[TELLO_NET], &[(FRIEND, TELLO)]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn two_drones_of_one_model() {
    let mut s = Sim::new();
    let other = Drone { bssid: TELLO2, ssid: b"TELLO-770102", boot_ms: 0, ch: 6, privacy: false };
    s.fly(1000, &[TELLO_NET, other], &[]);
    s.join(PILOT, TELLO);
    s.join(PILOT2, TELLO2);
    s.fly(20_000, &[TELLO_NET, other], &[(PILOT, TELLO), (PILOT2, TELLO2)]);
    // One of them lands and the phone leaves.
    s.mgmt(&kill(DEAUTH, TELLO2, PILOT2, TELLO2, 3));
    s.fly(5000, &[TELLO_NET, other], &[(PILOT, TELLO)]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
    assert_eq!(s.w.drones.len(), 2);
}

#[test]
fn reports_stop_and_networks_are_forgotten() {
    let mut s = Sim::new();
    s.fly(2000, &[TELLO_NET], &[(PILOT, TELLO)]);
    aireplay(&mut s, 4, TELLO, PILOT);
    let n = s.of(ThreatKind::DroneDeauth).len();
    assert!((1..=2).contains(&n), "{n}");
    s.tick(500);
    s.out.clear();
    for _ in 0..40 {
        s.tick(250);
    }
    assert!(s.drone_kinds().is_empty(), "stops when the frames stop");
    // Gone two minutes: forgotten.
    for _ in 0..130 {
        s.tick(1000);
    }
    assert!(s.w.drones.is_empty());
    // Deauths at a forgotten drone are not drone deauths any more.
    for _ in 0..5 {
        s.mgmt(&kill(DEAUTH, PILOT, TELLO, TELLO, 7));
    }
    s.tick(500);
    assert!(s.drone_kinds().is_empty());
}

#[test]
fn ordinary_networks_are_not_followed() {
    let mut s = Sim::new();
    for k in 0..50 {
        s.mgmt(&wpa2(HOME_AP, b"HomeNet"));
        s.mgmt(&kill(DEAUTH, PILOT, HOME_AP, HOME_AP, 7));
        if k % 10 == 0 {
            s.join(FRIEND, HOME_AP);
        }
        s.tick(100);
    }
    assert!(s.w.drones.is_empty());
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

/// The drone's beacon with a forged timestamp field (µs).
fn forged_beacon(d: &Drone, tsf_us: u64) -> Vec<u8> {
    let mut f = drone_beacon(d, 0);
    f[24..32].copy_from_slice(&tsf_us.to_le_bytes());
    f
}

#[test]
fn forged_low_timestamp_beacons_do_not_wipe_the_deauth_count() {
    // Same address, name, channel and security as the drone, timestamp 1:
    // until WP-11a each one "rebooted" the drone and emptied its deauth
    // ring, so three deauths in five seconds never added up.
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[(PILOT, TELLO)]);
    for _ in 0..3 {
        s.mgmt(&kill(DEAUTH, PILOT, TELLO, TELLO, 7));
        s.mgmt(&forged_beacon(&TELLO_NET, 1));
        s.fly(400, &[TELLO_NET], &[]);
    }
    s.tick(300);
    assert!(!s.of(ThreatKind::DroneDeauth).is_empty(), "{:?}", s.out);
}

#[test]
fn forged_reboot_does_not_hand_the_drone_to_the_attacker() {
    let mut s = Sim::new();
    s.fly(5000, &[TELLO_NET], &[(PILOT, TELLO)]);
    // One forged beacon, then the attacker's frames, then the pilot's.
    s.mgmt(&forged_beacon(&TELLO_NET, 1));
    s.data(&data(ATTACKER, TELLO, true));
    s.data(&data(ATTACKER, TELLO, true));
    s.fly(2000, &[TELLO_NET], &[(PILOT, TELLO), (ATTACKER, TELLO)]);
    assert_eq!(s.w.drones.nets[0].ctrl, PILOT, "the controller is still the pilot");
    let t = s.of(ThreatKind::DroneTakeover);
    assert!(!t.is_empty(), "{:?}", s.out);
    assert!(t.iter().all(|x| x.source == ATTACKER), "the alarm names the attacker, never the pilot: {t:?}");
}

#[test]
fn two_timelines_on_one_address_are_a_clone() {
    // A copy with the drone's own address, name and channel, keeping a
    // clock of its own (airbase-ng's beacons start at its own uptime).
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[]);
    let copy = Drone { boot_ms: 590_000, ..TELLO_NET };
    for _ in 0..4 {
        let b = drone_beacon(&copy, s.t + 30);
        s.mgmt(&b);
        s.fly(300, &[TELLO_NET], &[]);
    }
    let c = s.of(ThreatKind::DroneNetClone);
    assert!(!c.is_empty(), "{:?}", s.out);
    assert_eq!((c[0].source, c[0].target), (TELLO, TELLO));
    assert_ne!(c[0].detail & CLONE_SAME_ADDRESS, 0);
    assert_ne!(c[0].detail & CLONE_TWO_CLOCKS, 0);
    assert_eq!(c[0].detail & CLONE_OTHER_CHANNEL, 0);
    assert_eq!(c[0].ssid.as_str(), "TELLO-5A1B2C");
    // Stops when the copy does.
    s.fly(15_000, &[TELLO_NET], &[]);
    s.out.clear();
    s.fly(3000, &[TELLO_NET], &[]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn a_real_reboot_is_believed_after_its_beacons() {
    let mut s = Sim::new();
    s.fly(5000, &[TELLO_NET], &[(PILOT, TELLO)]);
    let old_boot = s.w.drones.nets[0].boot;
    s.tick(4000);
    let rebooted = Drone { boot_ms: s.t, ..TELLO_NET };
    // Fewer than REBOOT_BEACONS, or not yet REBOOT_CONFIRM_MS: not yet.
    s.fly(300, &[rebooted], &[]);
    assert_eq!(s.w.drones.nets[0].boot, old_boot);
    assert_eq!(s.w.drones.nets[0].ctrl, PILOT);
    s.fly(REBOOT_CONFIRM_MS as u64, &[rebooted], &[]);
    assert_ne!(s.w.drones.nets[0].boot, old_boot, "the new timeline is the drone's now");
    assert_eq!(s.w.drones.nets[0].ctrl, NONE, "a restart forgets the controller");
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn table_is_bounded() {
    let mut s = Sim::new();
    for i in 0..10u8 {
        let mut b = TELLO;
        b[5] = i;
        let name = std::format!("TELLO-5A1B{i:02X}");
        let mut f = beacon(b, name.as_bytes(), false, &[]);
        f[24] = 1;
        s.mgmt(&f);
    }
    assert_eq!(s.w.drones.len(), NETS);
}

/// `NETS` forged drone networks (DJI addresses, Tello names), each kept
/// "taken" the cheapest way: a data frame from one station, then
/// authentication requests from another, refreshed every two seconds.
fn fakes() -> Vec<Drone> {
    (0..NETS as u8)
        .map(|i| {
            let ssid: &'static [u8] = std::boxed::Box::leak(std::format!("TELLO-9900{i:02X}").into_bytes().into_boxed_slice());
            Drone { bssid: [0x60, 0x60, 0x1F, 0x99, 0x00, i], ssid, boot_ms: 0, ch: 6, privacy: false }
        })
        .collect()
}

fn keep_fakes_busy(s: &mut Sim, fakes: &[Drone]) {
    for (k, f) in fakes.iter().enumerate() {
        let a = [0x02, 0xAA, 0, 0, 0, k as u8];
        let b = [0x02, 0xBB, 0, 0, 0, k as u8];
        s.data(&data(a, f.bssid, true));
        s.mgmt(&mgmt(AUTH, f.bssid, b, f.bssid, &[0, 0, 1, 0, 0, 0]));
        s.mgmt(&mgmt(AUTH, f.bssid, b, f.bssid, &[0, 0, 1, 0, 0, 0]));
    }
}

#[test]
fn busy_forged_networks_do_not_lock_a_real_drone_out() {
    let mut s = Sim::new();
    let fakes = fakes();
    // The fakes take every slot and stay "taken".
    for _ in 0..10 {
        s.fly(1000, &fakes, &[]);
        keep_fakes_busy(&mut s, &fakes);
    }
    // The real drone arrives and flies; the fakes keep going.
    let mut all = fakes.clone();
    all.push(TELLO_NET);
    for _ in 0..90 {
        s.fly(1000, &all, &[(PILOT, TELLO)]);
        keep_fakes_busy(&mut s, &fakes);
    }
    assert!(s.w.drones.nets.iter().any(|n| n.bssid == TELLO), "the real drone is followed");
    assert!(s.w.drones.totals.evicted >= 1);
    // Its deauth is seen.
    s.out.clear();
    aireplay(&mut s, 2, TELLO, PILOT);
    s.tick(300);
    let d = s.of(ThreatKind::DroneDeauth);
    assert!(d.iter().any(|x| x.target == TELLO), "{:?}", s.out);
}

#[test]
fn a_spray_of_drone_beacons_keeps_partial_evidence() {
    // Two deauths of three at the real drone, then one beacon from each of
    // forty made-up drone networks, then the third deauth.
    let mut s = Sim::new();
    s.fly(3000, &[TELLO_NET], &[(PILOT, TELLO)]);
    s.mgmt(&kill(DEAUTH, PILOT, TELLO, TELLO, 7));
    s.mgmt(&kill(DEAUTH, PILOT, TELLO, TELLO, 7));
    for i in 0..40u8 {
        let b = [0x60, 0x60, 0x1F, 0x55, 0x00, i];
        let name = std::format!("TELLO-5500{i:02X}");
        let mut f = beacon(b, name.as_bytes(), false, &ie(3, &[6]));
        f[24] = 1;
        s.mgmt(&f);
    }
    s.mgmt(&kill(DEAUTH, PILOT, TELLO, TELLO, 7));
    s.tick(300);
    assert!(!s.of(ThreatKind::DroneDeauth).is_empty(), "{:?}", s.out);
}

// ------------------------------------------------------------- clones --

const COPY: [u8; 6] = [0x00, 0x00, 0x5E, 0x10, 0x20, 0x30];

#[test]
fn evil_twin_of_a_tello() {
    let mut s = Sim::new();
    s.fly(1000, &[TELLO_NET], &[]);
    let twin = Drone { bssid: COPY, ..TELLO_NET };
    s.mgmt(&drone_beacon(&twin, s.t));
    assert!(s.drone_kinds().is_empty(), "one frame is not enough");
    s.tick(100);
    s.mgmt(&drone_beacon(&twin, s.t));
    let c = s.of(ThreatKind::DroneNetClone);
    assert_eq!(c.len(), 1, "{:?}", s.out);
    assert_eq!((c[0].source, c[0].target), (COPY, TELLO));
    assert_eq!(c[0].ssid.as_str(), "TELLO-5A1B2C");
    assert_eq!(c[0].detail & 0xFF, CLONE_OPEN);
    assert_eq!(c[0].detail >> MAKER_SHIFT, 1);
    // Once a second while it lasts.
    s.fly(3000, &[TELLO_NET, twin], &[]);
    let n = s.of(ThreatKind::DroneNetClone).len();
    assert!((3..=5).contains(&n), "{n}");
    // The copy is not learned as a drone of its own (Tello names stand
    // alone, so it is; but it never confuses the original's controller).
    assert!(s.drone_kinds().iter().all(|k| *k == ThreatKind::DroneNetClone));
}

#[test]
fn open_copy_of_a_protected_drone() {
    let spark = Drone {
        bssid: [0x60, 0x60, 0x1F, 0xAF, 0x01, 0x02],
        ssid: b"Spark-45beb8",
        boot_ms: 0,
        ch: 11,
        privacy: true,
    };
    let mut s = Sim::new();
    s.fly(1000, &[spark], &[]);
    // Wifiphisher-style: open, other channel, a random address.
    let copy = Drone { bssid: [0x02, 0x9F, 0x01, 0x02, 0x03, 0x04], privacy: false, ch: 1, ..spark };
    s.mgmt(&drone_beacon(&copy, s.t));
    s.mgmt(&drone_beacon(&copy, s.t + 1));
    let c = s.of(ThreatKind::DroneNetClone);
    assert_eq!(c.len(), 1, "{:?}", s.out);
    let want = CLONE_OPEN | CLONE_SECURITY | CLONE_WRONG_MAKER | CLONE_OTHER_CHANNEL;
    assert_eq!(c[0].detail & 0xFF, want);
    assert_eq!(c[0].target, spark.bssid);
    assert_eq!(c[0].detail >> MAKER_SHIFT, 0, "DJI");
}

#[test]
fn drone_name_on_an_address_not_the_makers() {
    // A Spark's name from a laptop, the real Spark out of range.
    let mut s = Sim::new();
    let fake = Drone { bssid: COPY, ssid: b"Spark-45beb8", boot_ms: 0, ch: 6, privacy: true };
    s.mgmt(&drone_beacon(&fake, s.t));
    s.tick(100);
    s.mgmt(&drone_beacon(&fake, s.t));
    assert!(s.drone_kinds().is_empty(), "needs three frames without an original");
    s.tick(100);
    s.mgmt(&drone_beacon(&fake, s.t));
    let c = s.of(ThreatKind::DroneNetClone);
    assert_eq!(c.len(), 1, "{:?}", s.out);
    assert_eq!(c[0].target, NONE);
    assert_eq!(c[0].detail & 0xFF, CLONE_WRONG_MAKER);
    // Not learned as a drone either.
    assert!(s.w.drones.is_empty());
    // A weak name (no fixed digits) on a stranger's router is nothing.
    let mut s = Sim::new();
    for k in 0..20 {
        s.mgmt(&wpa2(HOME_AP, b"Typhoon"));
        s.mgmt(&wpa2(COPY, b"Bebop2_home"));
        s.tick(100 + k);
    }
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn same_address_on_another_channel() {
    let mut s = Sim::new();
    s.fly(1000, &[TELLO_NET], &[]);
    // airbase-ng with the drone's own address, on channel 1, while the
    // drone carries on on 6.
    let twin = Drone { ch: 1, ..TELLO_NET };
    for _ in 0..3 {
        s.mgmt(&drone_beacon(&twin, s.t + 10));
        s.fly(200, &[TELLO_NET], &[]);
    }
    let c = s.of(ThreatKind::DroneNetClone);
    assert!(!c.is_empty(), "{:?}", s.out);
    assert_eq!((c[0].source, c[0].target), (TELLO, TELLO));
    assert_ne!(c[0].detail & CLONE_SAME_ADDRESS, 0);
}

#[test]
fn drone_changing_channel_is_not_a_copy() {
    let mut s = Sim::new();
    s.fly(5000, &[TELLO_NET], &[(PILOT, TELLO)]);
    // Restarts on another channel (auto channel choice).
    s.tick(12_000);
    let moved = Drone { ch: 11, boot_ms: s.t, ..TELLO_NET };
    s.fly(5000, &[moved], &[(PILOT, TELLO)]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn dual_band_siblings_are_one_drone() {
    let a = Drone {
        bssid: [0x60, 0x60, 0x1F, 0x12, 0x34, 0x56],
        ssid: b"MavicAir-0a1b2c",
        boot_ms: 0,
        ch: 6,
        privacy: true,
    };
    let b = Drone { bssid: [0x60, 0x60, 0x1F, 0x12, 0x34, 0x57], ch: 149, ..a };
    let mut s = Sim::new();
    s.fly(5000, &[a, b], &[]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn identical_toys_without_a_unit_name() {
    let a = Drone { bssid: [0x00, 0x1C, 0x0A, 1, 2, 3], ssid: b"WiFi-720P", boot_ms: 0, ch: 6, privacy: false };
    let b = Drone { bssid: [0x00, 0x1C, 0x0A, 9, 8, 7], ..a };
    let mut s = Sim::new();
    s.fly(5000, &[a, b], &[]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
    // Both are still followed for deauth / takeover.
    assert_eq!(s.w.drones.len(), 2);
}

#[test]
fn drone_replaced_after_it_left() {
    // The original gone for more than CLONE_RECENT_MS: the next address
    // with the name is not a copy (the owner swapped the camera module).
    let mut s = Sim::new();
    s.fly(2000, &[TELLO_NET], &[]);
    s.tick(40_000);
    let other = Drone { bssid: TELLO2, ..TELLO_NET };
    s.fly(3000, &[other], &[]);
    assert!(s.drone_kinds().is_empty(), "{:?}", s.out);
}

#[test]
fn clone_table_survives_many_names() {
    let mut s = Sim::new();
    // Ten other drones' names pass by, then a copy of the Tello.
    s.fly(1000, &[TELLO_NET], &[]);
    for i in 0..10u8 {
        let mut b = TELLO2;
        b[5] = i;
        let name = std::format!("TELLO-AB{i:04X}");
        s.mgmt(&beacon(b, name.as_bytes(), false, &[]));
    }
    s.fly(1000, &[TELLO_NET, Drone { bssid: COPY, ..TELLO_NET }], &[]);
    assert!(!s.of(ThreatKind::DroneNetClone).is_empty(), "{:?}", s.out);
}
