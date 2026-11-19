//! Mutes and snoozes (WP-11b): a mute is one specific thing, an operator's
//! mute ends, the alarms that mean "the hub may be missing drones" can only
//! be snoozed, and nothing silenced ever clears "partly blind".

extern crate std;

use link::{ThreatKind, ThreatSighting};

use crate::site::{self, SiteLine, SiteState};
use crate::threats::{self, Mute, OPERATOR_MUTE_MS, SNOOZE_MS, ThreatId};
use crate::*;

fn sighting(kind: ThreatKind, source: u8, target: u8, freq: u32) -> ThreatSighting {
    ThreatSighting {
        kind,
        channel: 6,
        freq,
        width_khz: 0,
        level: -40,
        count: 50,
        window_ms: 1000,
        source: [0x02, 0, 0, 0, 0, source],
        target: [0x0a, 0, 0, 0, 0, target],
        detail: 7,
        ssid: heapless::String::new(),
        uptime_us: 1,
    }
}

fn blinded() -> ThreatSighting {
    let mut s = sighting(ThreatKind::Blinded, 0, 0, 0);
    s.source = [0; 6];
    s.target = [0; 6];
    s.channel = 0;
    s
}

fn twin(ssid: &str, source: u8) -> ThreatSighting {
    let mut s = sighting(ThreatKind::EvilTwin, source, 0, 24_370);
    s.ssid = heapless::String::try_from(ssid).unwrap();
    s
}

fn announced(tr: &mut Tracker, now: u64) -> std::vec::Vec<ThreatId> {
    let mut v = std::vec::Vec::new();
    while let Some(id) = tr.take_new_threat(now) {
        v.push(id);
    }
    v
}

fn kinds(tr: &Tracker, ids: &[ThreatId]) -> std::vec::Vec<(ThreatKind, u8)> {
    ids.iter().filter_map(|&i| tr.get_threat(i)).map(|t| (t.kind, t.node)).collect()
}

#[test]
fn snoozing_blinded_on_one_node_is_that_node_for_five_minutes() {
    let mut tr: Tracker = Tracker::default();
    // Node 1 is blinded (the hub's own deaf-node check raises it).
    let a = tr.threats.ingest(1, true, &blinded(), 1_000).unwrap();
    assert_eq!(announced(&mut tr, 1_000), [a]);
    assert!(tr.partly_blind(1_000));
    let m = tr.silence(a, 1_000).expect("Blinded can be snoozed");
    assert_eq!((m.kind, m.node), (ThreatKind::Blinded, 1), "this node, nothing wider");
    assert!(!m.permanent());
    assert_eq!(m.ends_ms(), Some(1_000 + SNOOZE_MS), "a snooze, not an operator mute");
    assert!(tr.get_threat(a).unwrap().snoozed);
    // Snoozed: no alert, but the hub may still be missing drones.
    assert!(tr.partly_blind(1_000));
    assert_eq!(tr.active_threats(1_000).count(), 0);

    // Node 2 goes deaf too: it alarms.
    let b = tr.threats.ingest(2, true, &blinded(), 2_000).unwrap();
    assert_ne!(a, b);
    let new = announced(&mut tr, 2_000);
    assert_eq!(kinds(&tr, &new), [(ThreatKind::Blinded, 2)]);
    assert!(tr.partly_blind(2_000));

    // Node 1 stays deaf; once the snooze ends it alarms again, and it was
    // never anything but partly blind.
    let mut t = 2_000;
    while t < 1_000 + SNOOZE_MS {
        t += 5_000;
        tr.threats.ingest(1, true, &blinded(), t);
        tr.threats.ingest(2, true, &blinded(), t);
        tr.expire(t);
        assert!(tr.partly_blind(t), "{t}");
        if t < 1_000 + SNOOZE_MS {
            assert!(announced(&mut tr, t).is_empty(), "{t}");
        }
    }
    t += 1_000;
    tr.threats.ingest(1, true, &blinded(), t);
    tr.expire(t);
    assert_eq!(announced(&mut tr, t), [a], "alarms again after the snooze");
    assert!(!tr.get_threat(a).unwrap().muted);
    assert!(tr.partly_blind(t));
}

#[test]
fn mute_all_never_silences_jamming_or_unseen_alarms() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_threat(1, &sighting(ThreatKind::Deauth, 1, 0xff, 0), 1_000);
    tr.ingest_threat(1, &twin("CafeWiFi", 7), 1_000);
    let jam = tr.ingest_threat(2, &sighting(ThreatKind::JamBroadband, 0, 0, 24_370), 1_000).unwrap();
    assert_eq!(announced(&mut tr, 1_000).len(), 3);
    // An alarm raised after the operator looked, and one not yet handed
    // out for an alert, are not in "Mute N".
    tr.ingest_threat(3, &sighting(ThreatKind::BeaconFlood, 4, 0xff, 0), 1_500);
    assert_eq!(tr.alarm_counts(1_000, 1_500), (2, 1), "two nuisance alarms to mute, one to snooze");
    let rules = tr.mute_seen(1_000, 1_500);
    assert_eq!(rules.len(), 2);
    assert!(rules.iter().all(|m| !threats::is_safety_kind(m.kind)));
    // The jamming alarm still sounds and the hub is still partly blind.
    assert!(tr.active_threats(1_500).any(|t| t.id == jam));
    assert!(tr.partly_blind(1_500));
    assert!(tr.active_threats(1_500).any(|t| t.kind == ThreatKind::BeaconFlood), "not seen yet");
    // Every rule is specific: the deauth flood on node 1 only.
    let deauth = rules.iter().find(|m| m.kind == ThreatKind::Deauth).unwrap();
    assert_eq!(deauth.node, 1);
    tr.ingest_threat(5, &sighting(ThreatKind::Deauth, 1, 0xff, 0), 2_000);
    assert!(tr.active_threats(2_000).any(|t| t.kind == ThreatKind::Deauth && t.node == 5));
    // Snoozing is a separate, explicit act.
    assert_eq!(tr.snooze_seen(1_000, 2_000).len(), 1);
    assert!(tr.partly_blind(2_000), "a snoozed jammer still makes the hub partly blind");
}

#[test]
fn operator_mutes_expire_site_mutes_stay_and_old_catch_alls_are_dropped() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_threat(1, &twin("CafeWiFi", 7), 1_000).unwrap();
    announced(&mut tr, 1_000);
    let m = tr.silence(a, 1_000).unwrap();
    assert_eq!(m.ends_ms(), Some(1_000 + OPERATOR_MUTE_MS));
    assert_eq!((m.ssid.as_str(), m.source[5]), ("CafeWiFi", 7));
    // Another copy of the name from another transmitter is not muted.
    tr.ingest_threat(1, &twin("CafeWiFi", 8), 2_000);
    assert_eq!(announced(&mut tr, 2_000).len(), 1);
    // Still heard two hours later: the mute ends and it alarms again.
    // Meanwhile each time it comes back is recorded, silenced (no alert).
    let end = 1_000 + OPERATOR_MUTE_MS;
    let mut t = 2_000;
    while t < end - 1_000 {
        t += 60_000;
        tr.ingest_threat(1, &twin("CafeWiFi", 7), t.min(end - 1_000));
        assert_eq!(announced(&mut tr, t), [a]);
        assert_eq!(tr.get_threat(a).unwrap().silence(), Some(threats::Silence::Muted));
        assert_eq!(tr.active_threats(t.min(end - 1_000)).count(), 0, "no alert");
    }
    assert!(tr.get_threat(a).unwrap().muted);
    tr.ingest_threat(1, &twin("CafeWiFi", 7), end + 1_000);
    assert!(!tr.get_threat(a).unwrap().muted);
    assert!(announced(&mut tr, end + 1_000).contains(&a));
    assert!(tr.mutes().is_empty());

    // A venue's guest network named like its secured one: one explicit line
    // in the site file mutes it for good.
    let SiteLine::Mute(venue) = site::parse_line("mute evil_twin source=02:00:00:00:00:09 ssid=Venue Guest").unwrap() else {
        panic!()
    };
    assert!(venue.permanent());
    assert_eq!(tr.mute(venue, end), Ok(true));
    let v = tr.ingest_threat(2, &twin("Venue Guest", 9), end + 2_000).unwrap();
    assert!(tr.get_threat(v).unwrap().muted);
    // Days later (a new entry by then): still muted, never alerted on, but
    // its raise is recorded as muted by the site file.
    let later = end + 10 * OPERATOR_MUTE_MS;
    let v = tr.ingest_threat(2, &twin("Venue Guest", 9), later).unwrap();
    tr.expire(later);
    assert!(tr.get_threat(v).unwrap().muted, "for good");
    assert!(announced(&mut tr, later).contains(&v));
    assert_eq!(tr.get_threat(v).unwrap().silence(), Some(threats::Silence::ForGood));
    assert!(tr.active_threats(later).all(|t| t.id != v), "no alert");

    // Rules an older hub saved on the card: the catch-alls and the kinds
    // that can only be snoozed are not honoured, with a reason.
    for old in [
        "mute blinded",
        "mute jam_broadband mhz=2437.0",
        "mute area_gnss target=02:00:00:00:00:00",
        "mute deauth",
        "mute control_contested mhz=915.5",
        "mute evil_twin source=02:00:00:00:00:09",
        "mute id_flood",
        "mute forged_link source=02:00:00:00:00:01",
    ] {
        let e = site::parse_line(old).expect_err(old);
        assert!(site::is_dropped_mute(e), "{old}: {e}");
    }
    let mut st = SiteState::default();
    let mut lines = site::Lines::default();
    let mut refused = std::vec::Vec::new();
    site::read_into(&mut st, &mut lines, b"mute blinded\nmute deauth target=ff:ff:ff:ff:ff:ff\nmute deauth\n", |n, e| refused.push((n, e)));
    assert_eq!((st.mutes.len(), st.mutes_dropped, refused.len()), (1, 2, 2));
    // Built by hand, a catch-all is refused by the tracker too.
    let catch_all = Mute { kind: ThreatKind::Blinded, ..Mute::empty(ThreatKind::Blinded) };
    assert!(tr.mute(catch_all, later).is_err());
    let blind = tr.threats.ingest(3, true, &blinded(), later).unwrap();
    assert!(!tr.get_threat(blind).unwrap().muted);
}

#[test]
fn a_contested_link_without_identifiers_mutes_only_that_link() {
    use ctrlwatch::contested::{RULE_PHASE, encode_detail};
    use ctrlwatch::{Region, System};
    let mut tr: Tracker = Tracker::default();
    let mut s = sighting(ThreatKind::ControlContested, 1, 0, 9_155);
    s.target = [0; 6];
    s.source = [0, 0, 0, 0, 0, 1];
    s.detail = encode_detail(System::ExpressLrs, 2, RULE_PHASE, Region::Fcc915);
    let a = tr.ingest_threat(4, &s, 1_000).unwrap();
    announced(&mut tr, 1_000);
    tr.silence(a, 1_000).unwrap();
    // Another link in the same band, same node: not muted.
    let mut s2 = s.clone();
    s2.source[5] = 2;
    tr.ingest_threat(4, &s2, 2_000);
    assert_eq!(announced(&mut tr, 2_000).len(), 1);
}
