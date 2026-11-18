//! Impersonation (WP-10, docs/SPOOFING.md "Impersonation and disputes").
//!
//! The attacker transmits well-formed frames carrying a REAL drone's ID
//! (serials are broadcast in clear) with any other values. One forged frame
//! must not move, rename or merge a real drone; a forger that keeps it up is
//! shown as a dispute over the ID, not as the real drone spoofing. And the
//! honest cases that look alike (one drone on several transmitters, a
//! rotating Bluetooth address, a new session ID, a GPS glitch, two nodes)
//! must keep working. Every serial and address here is made up.

extern crate std;
use std::vec::Vec as SVec;

use super::*;
use crate::spoof::{Check, REPORT_MIN};
use crate::tests::CITY_HALL;
use link::{DroneIdRecord, DroneIdSighting};
use rid::message::*;

const SN: &str = "1ZZZ9FAKE00001";
const SN_Z: &str = "1ZZZ9FAKE00099";
const MAC_A: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x01];
const MAC_B: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x02];
/// The forger's own address.
const MAC_F: [u8; 6] = [0x02, 0xf0, 0xf0, 0xf0, 0xf0, 0x0f];
const BLE_A: [u8; 6] = [0xc3, 0x01, 0x02, 0x03, 0x04, 0x05];
const BLE_B: [u8; 6] = [0xd7, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e];

fn at(east_m: f64, north_m: f64) -> LatLon {
    LatLon {
        lat: CITY_HALL.0 + north_m / 111_132.0,
        lon: CITY_HALL.1 + east_m / (111_132.0 * CITY_HALL.0.to_radians().cos()),
    }
}

fn dist(a: LatLon, b: LatLon) -> f64 {
    crate::spoof::dist_m(a, b)
}

/// The real drone: 5 m/s east.
fn real_at(ms: u64) -> LatLon {
    at(200.0 + 5.0 * ms as f64 / 1000.0, 300.0)
}

/// The forger's claimed path: 2 km away, 3 m/s north, consistent.
fn fake_at(ms: u64) -> LatLon {
    at(2_000.0, 1_500.0 + 3.0 * ms as f64 / 1000.0)
}

fn basic(sn: &str) -> Message {
    Message::BasicId(BasicId {
        id_type: IdType::SerialNumber,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(sn.as_bytes()).unwrap(),
    })
}

fn session(n: u8) -> Message {
    Message::BasicId(BasicId {
        id_type: IdType::SpecificSession,
        ua_type: UaType::HelicopterOrMultirotor,
        uas_id: FixedStr::new(&[0x01, 0xAB, n, 0x10, 0x20]).unwrap(),
    })
}

fn loc(p: LatLon, ms: u64) -> Message {
    Message::Location(Location {
        status: Status::Airborne,
        height_type: HeightType::AboveTakeoff,
        direction: Some(90.0),
        speed_horizontal: Some(5.0),
        speed_vertical: Some(0.0),
        position: Some(p),
        altitude_pressure: Some(80.0),
        altitude_geodetic: Some(100.0),
        height: Some(50.0),
        horizontal_accuracy: HorizontalAccuracy::M3,
        vertical_accuracy: VerticalAccuracy::M10,
        baro_accuracy: VerticalAccuracy::M10,
        speed_accuracy: SpeedAccuracy::Mps1,
        timestamp: Some(((ms / 100) % 36_000) as f32 / 10.0),
        timestamp_accuracy: Some(0.2),
    })
}

fn sys(op: LatLon) -> Message {
    Message::System(System {
        operator_location_type: OperatorLocationType::Takeoff,
        classification_type: ClassificationType::Undeclared,
        operator_position: Some(op),
        area_count: 1,
        area_radius: 0,
        area_ceiling: None,
        area_floor: None,
        category: Category::Undeclared,
        class: UaClass::Undeclared,
        operator_altitude: Some(15.0),
        timestamp: Some(300_000_000),
    })
}

fn self_id(text: &str) -> Message {
    Message::SelfId(SelfId {
        desc_type: DescType::Text,
        description: FixedStr::new(text.as_bytes()).unwrap(),
    })
}

fn op_id(text: &str) -> Message {
    Message::OperatorId(OperatorId {
        operator_id_type: OperatorIdType::OperatorId,
        operator_id: FixedStr::new(text.as_bytes()).unwrap(),
    })
}

fn feed(tr: &mut Tracker, node: u8, transport: Transport, mac: [u8; 6], msgs: &[Message], ms: u64) -> Ingested {
    let obs = Observation {
        node,
        transport,
        channel: if matches!(transport, Transport::WifiBeacon | Transport::WifiNan) { 6 } else { 37 },
        rssi: -70,
        mac,
        counter: (ms / 250) as u8,
        rx_ms: ms,
    };
    tr.ingest_messages(&obs, msgs, 0)
}

fn by_serial<'a>(tr: &'a Tracker, sn: &str) -> &'a Track {
    tr.tracks()
        .iter()
        .find(|t| t.uas_ids.iter().any(|u| u.id.as_str() == Some(sn)))
        .unwrap_or_else(|| panic!("no track with {sn}: {:?}", tr.tracks()))
}

fn pos(t: &Track) -> LatLon {
    t.location.as_ref().and_then(|l| l.value.position).expect("a position")
}

fn hub() -> Tracker {
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), 0);
    tr
}

fn real_frame(tr: &mut Tracker, ms: u64) -> Ingested {
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
    feed(tr, 0, Transport::WifiBeacon, MAC_A, &[basic(SN), loc(real_at(ms), ms), sys(at(0.0, 0.0))], ms)
}

fn forged_frame(tr: &mut Tracker, ms: u64) -> Ingested {
    feed(
        tr,
        0,
        Transport::WifiBeacon,
        MAC_F,
        &[basic(SN), loc(fake_at(ms), ms), sys(at(2_100.0, 1_400.0)), self_id("FORGED")],
        ms,
    )
}

fn drain(tr: &mut Tracker, ms: u64) -> SVec<SpoofEvent> {
    let mut v = SVec::new();
    while let Some(e) = tr.take_spoof_flag(ms) {
        v.push(e);
    }
    v
}

/// The real drone's position (as shown) is on its own path: within 30 m of
/// where it really was when that report arrived.
fn assert_on_real_path(t: &Track, when: u64) {
    let l = t.location.as_ref().unwrap();
    let d = dist(l.value.position.unwrap(), real_at(l.rx_ms));
    assert!(d < 30.0, "at {when}: shown position {d:.0} m off the real path");
}

// ------------------------------------------------------------ forgers --

/// A forger repeats the real drone's serial with a position 2 km away,
/// alternating with the real drone for 60 s.
#[test]
fn forged_twin_alternating_for_60_s_never_moves_the_real_drone() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 5_000 {
        real_frame(&mut tr, ms);
        ms += 500;
    }
    let id = by_serial(&tr, SN).id;
    while ms < 65_000 {
        assert_eq!(real_frame(&mut tr, ms).track(), Some(id));
        assert_on_real_path(tr.get(id).unwrap(), ms);
        forged_frame(&mut tr, ms + 250);
        let t = tr.get(id).unwrap();
        assert_on_real_path(t, ms + 250);
        // The operator position and the text fields stay the real ones.
        let op = t.system.as_ref().unwrap().value.operator_position.unwrap();
        assert!(dist(op, at(0.0, 0.0)) < 1.0, "operator moved at {ms}");
        assert!(t.self_id.is_none(), "the forger's Self-ID was applied at {ms}");
        ms += 500;
    }
    assert_eq!(tr.len(), 1, "{:?}", tr.tracks());
    assert!(drain(&mut tr, ms).is_empty(), "no spoof event against the real drone");
    let t = tr.get(id).unwrap();
    assert!(t.spoof.flag(Check::PositionJump).is_none(), "{:?}", t.spoof.flags());
    assert!(t.spoof.flag(Check::AddressAlternation).is_none(), "{:?}", t.spoof.flags());
    assert!(t.spoof.score() < REPORT_MIN, "{:?}", t.spoof.flags());
    // A dispute, naming both claims. (The forger's Self-ID differs from
    // nothing: the real drone sends none, and it was not applied.)
    let d = t.claims.dispute(ms).expect("disputed");
    assert_eq!(d.what, claims::what::POSITION);
    let r = d.rival.unwrap();
    assert!(dist(r.position, fake_at(ms - 250)) < 30.0);
    assert_eq!(r.src, ClaimSrc::RemoteId);
    assert!(!d.followed);
    let ev = tr.take_dispute(ms).expect("one evidence event");
    assert_eq!(ev.track, id);
    let text = claims::event_text(&ev, tr.get(id));
    assert!(text.starts_with("track "), "{text}");
    assert!(text.contains("SerialNumber 1ZZZ9FAKE00001: two transmitters claim this ID; another Remote ID claim"), "{text}");
    assert!(text.ends_with("m from the shown one. Neither is verified."), "{text}");
    assert!(!text.contains("spoof"), "worded as a dispute: {text}");
    assert_eq!(tr.take_dispute(ms), None, "once per dispute");
    // The forger stops: the dispute ends after RIVAL_TTL_MS.
    let quiet = ms + u64::from(claims::RIVAL_TTL_MS);
    real_frame(&mut tr, quiet);
    assert!(!tr.get(id).unwrap().claims.disputed(quiet));
    assert!(tr.get(id).unwrap().claims.ever_disputed());
}

/// The dispute is raised within a few seconds of the forger starting.
#[test]
fn a_dispute_is_raised_within_seconds() {
    let mut tr = hub();
    let mut ms = 0;
    let mut raised = None;
    while ms < 20_000 {
        real_frame(&mut tr, ms);
        if ms >= 5_000 {
            forged_frame(&mut tr, ms + 250);
        }
        if raised.is_none() && tr.take_dispute(ms + 250).is_some() {
            raised = Some(ms + 250);
        }
        ms += 500;
    }
    // Three rival reports over 2 s with the original between them.
    assert_eq!(raised, Some(7_250));
}

/// The original stops; only the forger continues, consistently: the track
/// follows it, but only after `claims::FOLLOW_AFTER_MS` (10 s).
#[test]
fn rival_alone_is_followed_after_the_stated_time() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 20_000 {
        real_frame(&mut tr, ms);
        if ms >= 5_000 {
            forged_frame(&mut tr, ms + 250);
        }
        ms += 500;
    }
    let id = by_serial(&tr, SN).id;
    assert!(tr.get(id).unwrap().claims.disputed(ms));
    let last_real = ms - 500;
    while ms < 45_000 {
        forged_frame(&mut tr, ms + 250);
        let t = tr.get(id).unwrap();
        if ms + 250 < last_real + claims::FOLLOW_AFTER_MS {
            assert!(dist(pos(t), real_at(last_real)) < 1.0, "followed too soon, at {ms}");
        } else if ms + 250 >= last_real + claims::FOLLOW_AFTER_MS + 1_000 {
            assert!(dist(pos(t), fake_at(ms + 250)) < 30.0, "not followed at {ms}");
            // It was a dispute, so following is said (until the dispute
            // ends, RIVAL_TTL_MS after the rival was last heard as one).
            // (`last` is kept to whole seconds of the dispute: up to 1 s early.)
            if ms + 250 < last_real + claims::FOLLOW_AFTER_MS + u64::from(claims::RIVAL_TTL_MS) - 1_000 {
                assert!(t.claims.dispute(ms + 250).unwrap().followed, "at {ms}");
                let s: heapless::String<192> = crate::text::render(claims::Summary(t, ms + 250));
                assert!(s.ends_with("now showing the claim that continued"), "{s}");
            }
        }
        ms += 500;
    }
    // No jump is flagged: it was a dispute, not one transmitter jumping.
    let t = tr.get(id).unwrap();
    assert!(t.spoof.flag(Check::PositionJump).is_none());
    assert!(t.claims.ever_disputed());
}

/// One transmitter that really jumps 3 km and carries on from there: the
/// track follows after the stated time, and that is a position jump.
#[test]
fn one_transmitter_that_jumps_is_followed_and_flagged() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 40_000 {
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        let p = if ms < 10_000 { real_at(ms) } else { fake_at(ms) };
        feed(&mut tr, 0, Transport::WifiBeacon, MAC_A, &[basic(SN), loc(p, ms)], ms);
        ms += 500;
    }
    let t = by_serial(&tr, SN);
    assert!(dist(pos(t), fake_at(ms - 500)) < 30.0);
    assert!(!t.claims.ever_disputed());
    let f = t.spoof.flag(Check::PositionJump).expect("a jump");
    assert_eq!(f.confidence, 25, "one jump: weak");
    assert!(f.detail > 2_000, "{f:?}");
    // One that keeps jumping to random places, with nothing on its old
    // path, is flagged stronger each time (no path to follow, no dispute).
    let mut k = 0.0;
    while ms < 60_000 {
        k += 1.0;
        let p = at(-3_000.0 * k, 900.0 * k);
        feed(&mut tr, 0, Transport::WifiBeacon, MAC_A, &[basic(SN), loc(p, ms)], ms);
        ms += 500;
    }
    let t = by_serial(&tr, SN);
    assert!(t.spoof.flag(Check::PositionJump).unwrap().confidence >= 75);
    assert!(!t.claims.ever_disputed());
}

/// One frame naming two drones' IDs (A's serial, B's session ID) does not
/// fold them together, repeated or not; neither track gains the other's ID.
#[test]
fn a_frame_naming_two_drones_does_not_merge_them() {
    let mut tr = hub();
    let b_at = |ms: u64| at(1_500.0, -800.0 + ms as f64 / 1000.0);
    let mut ms = 0;
    while ms < 3_000 {
        real_frame(&mut tr, ms);
        feed(&mut tr, 1, Transport::WifiBeacon, MAC_B, &[session(7), loc(b_at(ms), ms)], ms + 100);
        ms += 500;
    }
    for k in 0..6u64 {
        feed(&mut tr, 0, Transport::WifiBeacon, MAC_F, &[basic(SN), session(7)], ms + k * 500 + 200);
    }
    while ms < 9_000 {
        real_frame(&mut tr, ms);
        feed(&mut tr, 1, Transport::WifiBeacon, MAC_B, &[session(7), loc(b_at(ms), ms)], ms + 100);
        ms += 500;
    }
    assert_eq!(tr.len(), 2, "{:?}", tr.tracks());
    let a = by_serial(&tr, SN);
    assert_eq!(a.uas_ids.len(), 1, "{:?}", a.uas_ids);
    assert_on_real_path(a, ms);
    let b = tr.tracks().iter().find(|t| t.id != a.id).unwrap();
    assert_eq!(b.uas_ids.len(), 1);
    assert_eq!(b.uas_ids[0].id_type, IdType::SpecificSession);
    assert!(dist(pos(b), b_at(ms)) < 30.0);
}

/// The honest version: one drone sends its serial on Wi-Fi and its session
/// ID on Bluetooth (two tracks at first), then a frame with both, from the
/// same place, repeatedly: merged after `claims::MERGE_SPAN_MS`.
#[test]
fn one_drone_naming_both_its_ids_is_merged_after_repeated_agreement() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 3_000 {
        real_frame(&mut tr, ms);
        feed(&mut tr, 1, Transport::BleLongRange, BLE_A, &[session(7), loc(real_at(ms), ms)], ms + 100);
        ms += 500;
    }
    assert_eq!(tr.len(), 2);
    let a = by_serial(&tr, SN).id;
    let mut merged_at = None;
    while ms < 8_000 {
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        feed(&mut tr, 1, Transport::BleLongRange, BLE_A, &[basic(SN), session(7), loc(real_at(ms), ms)], ms + 100);
        if merged_at.is_none() && tr.len() == 1 {
            merged_at = Some(ms + 100);
        }
        ms += 500;
    }
    assert_eq!(merged_at, Some(5_100), "three frames over 2 s");
    assert_eq!(tr.tracks()[0].id, a, "the older track survives");
    assert_eq!(tr.tracks()[0].uas_ids.len(), 2);
}

/// A's serial sent from B's address (B a Bluetooth drone known only by
/// its address so far, flying elsewhere) does not fold B into A, and B's
/// later frames do not move A.
#[test]
fn serial_from_another_drones_address_does_not_fold_it() {
    let mut tr = hub();
    let b_at = |ms: u64| at(1_500.0, -800.0 + ms as f64 / 1000.0);
    let mut ms = 0;
    while ms < 3_000 {
        real_frame(&mut tr, ms);
        feed(&mut tr, 1, Transport::BleLegacy, BLE_B, &[loc(b_at(ms), ms)], ms + 100);
        feed(&mut tr, 1, Transport::BleLegacy, BLE_B, &[loc(b_at(ms + 250), ms + 250)], ms + 350);
        ms += 500;
    }
    assert_eq!(tr.len(), 2);
    feed(&mut tr, 1, Transport::BleLegacy, BLE_B, &[basic(SN)], ms + 50);
    while ms < 8_000 {
        real_frame(&mut tr, ms);
        feed(&mut tr, 1, Transport::BleLegacy, BLE_B, &[loc(b_at(ms), ms)], ms + 100);
        assert_on_real_path(by_serial(&tr, SN), ms);
        ms += 500;
    }
    assert_eq!(tr.len(), 2, "{:?}", tr.tracks());
    let a = by_serial(&tr, SN).id;
    let b = tr.tracks().iter().find(|t| t.id != a).unwrap();
    assert!(dist(pos(b), b_at(ms - 500)) < 30.0);
}

/// A frame with two serials renames nothing: A keeps its serial and its
/// track, and nothing takes the second serial from it.
#[test]
fn two_serials_in_one_frame_rename_nothing() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 3_000 {
        real_frame(&mut tr, ms);
        ms += 500;
    }
    let id = by_serial(&tr, SN).id;
    feed(&mut tr, 0, Transport::WifiBeacon, MAC_F, &[basic(SN), basic(SN_Z), loc(real_at(ms), ms)], ms);
    let t = tr.get(id).unwrap();
    assert_eq!(t.uas_ids.len(), 1);
    assert_eq!(t.uas_ids[0].id.as_str(), Some(SN));
    assert_eq!(real_frame(&mut tr, ms + 500).track(), Some(id));
    assert_eq!(tr.len(), 1);
    assert!(tr.tracks().iter().all(|t| t.uas_ids.iter().all(|u| u.id.as_str() != Some(SN_Z))));
    // Recorded, not acted on.
    assert_eq!(tr.contradictory_frames(), 1);
    assert_eq!(tr.get(id).unwrap().claims.contradictory(), 1);
    assert!(tr.take_dispute(ms + 500).is_none());
}

/// Forged Self-ID and operator ID from a transmitter at the real drone's
/// own position (so its position is plausible) never silently replace the
/// real ones.
#[test]
fn forged_text_fields_never_replace_the_real_ones() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 30_000 {
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        feed(
            &mut tr,
            0,
            Transport::WifiBeacon,
            MAC_A,
            &[basic(SN), loc(real_at(ms), ms), self_id("Survey flight"), op_id("FIN87astrdge12k8")],
            ms,
        );
        if ms >= 3_000 {
            feed(
                &mut tr,
                0,
                Transport::WifiBeacon,
                MAC_F,
                &[basic(SN), loc(real_at(ms + 250), ms + 250), self_id("POLICE"), op_id("FAKE-OPERATOR")],
                ms + 250,
            );
        }
        let t = by_serial(&tr, SN);
        assert_eq!(t.self_id.as_ref().unwrap().value.description.as_str(), Some("Survey flight"), "at {ms}");
        assert_eq!(t.operator_id.as_ref().unwrap().value.operator_id.as_str(), Some("FIN87astrdge12k8"), "at {ms}");
        ms += 500;
    }
    // The alternation is a dispute over the text fields (no position one).
    let d = by_serial(&tr, SN).claims.dispute(ms).expect("disputed");
    assert_eq!(d.what, claims::what::SELF_ID | claims::what::OPERATOR_ID);
    assert!(d.rival.is_none());
}

/// An operator who edits the Self-ID mid-flight: the new text is shown once
/// it has been sent for `claims::ADOPT_MS`, with no dispute.
#[test]
fn a_changed_self_id_is_adopted_when_it_persists() {
    let mut tr = hub();
    let mut ms = 0;
    let mut shown_at = None;
    while ms < 20_000 {
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        let text = if ms < 10_000 { "Inspection" } else { "Inspection, roof" };
        feed(&mut tr, 0, Transport::WifiBeacon, MAC_A, &[basic(SN), loc(real_at(ms), ms), self_id(text)], ms);
        let t = by_serial(&tr, SN);
        if shown_at.is_none() && t.self_id.as_ref().unwrap().value.description.as_str() == Some("Inspection, roof") {
            shown_at = Some(ms);
        }
        ms += 500;
    }
    assert_eq!(shown_at, Some(10_000 + u64::from(claims::ADOPT_MS)));
    assert!(!by_serial(&tr, SN).claims.ever_disputed());
}

/// A legacy DJI Wi-Fi DroneID element with the drone's serial and a
/// position 1 km away, while Remote ID is briefly quiet, does not move it.
#[test]
fn forged_dji_wifi_droneid_does_not_move_a_quiet_remote_id_drone() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 10_000 {
        real_frame(&mut tr, ms);
        ms += 500;
    }
    let id = by_serial(&tr, SN).id;
    // Remote ID quiet for 6 s; the forger sends DJI Wi-Fi DroneID.
    let far = at(-600.0, 1_200.0);
    for k in 0..6u64 {
        let t = ms + 1_000 + k * 1_000;
        tr.ingest_dji_wifi(0, 6, -60, MAC_F, &dji_element(SN, far), t);
        assert!(dist(pos(tr.get(id).unwrap()), far) > 500.0, "moved by DJI Wi-Fi at {t}");
        assert_eq!(tr.get(id).unwrap().claims.rival(t).unwrap().src, ClaimSrc::DjiWifi);
    }
    ms += 7_000;
    while ms < 20_000 {
        real_frame(&mut tr, ms);
        assert_on_real_path(tr.get(id).unwrap(), ms);
        ms += 500;
    }
}

/// A plaintext OcuSync DroneID with the drone's serial, 1.2 km away and
/// alternating with Remote ID, never moves it and raises no spoof flag on
/// it (it is a dispute).
#[test]
fn forged_plaintext_droneid_does_not_move_remote_id() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 3_000 {
        real_frame(&mut tr, ms);
        ms += 500;
    }
    let id = by_serial(&tr, SN).id;
    while ms < 40_000 {
        real_frame(&mut tr, ms);
        // Every 2.5 s, 0.3 s after a frame: stale enough to be shown if
        // nothing checked it.
        if ms % 2_500 == 0 {
            tr.ingest_droneid(2, &droneid(SN, at(1_400.0, 300.0)), ms + 300);
        }
        assert_on_real_path(tr.get(id).unwrap(), ms);
        ms += 500;
    }
    assert!(drain(&mut tr, ms).is_empty());
    let t = tr.get(id).unwrap();
    assert!(t.spoof.score() < REPORT_MIN, "{:?}", t.spoof.flags());
    let d = t.claims.dispute(ms).expect("disputed");
    assert_eq!(d.rival.unwrap().src, ClaimSrc::DroneId);
}

/// One decrypted O4 record naming a real drone's serial, with a position
/// 2 km from it (a bit-flipped or minted record), does not fold the O4
/// session into the drone's track or move it.
#[test]
fn one_decrypted_o4_record_does_not_merge_or_move() {
    let k = [0xde, 0xad, 0xbe, 0xef];
    let mut tr = hub();
    let mut ms = 0;
    while ms < 3_000 {
        real_frame(&mut tr, ms);
        ms += 500;
    }
    let id = by_serial(&tr, SN).id;
    tr.ingest_droneid(2, &o4(link::O4Kind::Cryp, k), ms);
    tr.ingest_droneid(2, &o4(link::O4Kind::Infp10, k), ms + 100);
    let p = infp10(SN, fake_at(ms), 5);
    tr.ingest_o4_decrypted(k, crate::O4Decrypted::Infp10(&p), ms + 100);
    assert_eq!(tr.len(), 2, "session merged on one record");
    assert_on_real_path(tr.get(id).unwrap(), ms);
    // Kept up (minted with the receiver's public key, or a replayed and
    // bit-flipped session), it never joins and never moves the drone: it
    // is a dispute over the serial, labelled as decrypted O4.
    for k in 1..20u64 {
        let t = ms + k * 500;
        real_frame(&mut tr, t);
        tr.ingest_droneid(2, &o4(link::O4Kind::Infp10, k4()), t + 100);
        let p = infp10(SN, fake_at(t), 5 + k as u16);
        tr.ingest_o4_decrypted(k4(), crate::O4Decrypted::Infp10(&p), t + 100);
        assert_on_real_path(tr.get(id).unwrap(), t);
    }
    assert_eq!(tr.len(), 2);
    let d = tr.get(id).unwrap().claims.dispute(ms + 10_000).expect("disputed");
    assert_eq!(d.rival.unwrap().src, ClaimSrc::DroneIdO4);
    assert_eq!(ClaimSrc::DroneIdO4.label(), "DJI O4, decrypted, not authenticated");
}

fn k4() -> [u8; 4] {
    [0xde, 0xad, 0xbe, 0xef]
}

/// An honest O4 drone (decrypted records agree with its Remote ID) joins
/// its Remote ID track after three records, and is never disputed.
#[test]
fn honest_decrypted_o4_joins_and_agrees() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 3_000 {
        real_frame(&mut tr, ms);
        ms += 500;
    }
    let id = by_serial(&tr, SN).id;
    tr.ingest_droneid(2, &o4(link::O4Kind::Cryp, k4()), ms);
    for k in 0..10u64 {
        let t = ms + k * 500;
        real_frame(&mut tr, t);
        tr.ingest_droneid(2, &o4(link::O4Kind::Infp10, k4()), t + 100);
        let p = infp10(SN, real_at(t + 100), k as u16);
        tr.ingest_o4_decrypted(k4(), crate::O4Decrypted::Infp10(&p), t + 100);
    }
    assert_eq!(tr.len(), 1);
    let t = tr.get(id).unwrap();
    assert!(t.dji.o4_decrypted);
    assert!(!t.claims.ever_disputed());
    assert_on_real_path(t, ms + 5_000);
}

// --------------------------------------------------- must still work --

/// One drone on a Wi-Fi beacon, NAN and Bluetooth Long Range, from three
/// addresses, same ID and position: one track, clean.
#[test]
fn one_drone_on_beacon_nan_and_bluetooth_is_one_track() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 60_000 {
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        let f = [basic(SN), loc(real_at(ms), ms), sys(at(0.0, 0.0))];
        feed(&mut tr, 0, Transport::WifiBeacon, MAC_A, &f, ms);
        feed(&mut tr, 0, Transport::WifiNan, MAC_B, &f, ms + 30);
        feed(&mut tr, 1, Transport::BleLongRange, BLE_A, &f, ms + 60);
        ms += 500;
    }
    assert_eq!(tr.len(), 1, "{:?}", tr.tracks());
    let t = &tr.tracks()[0];
    assert!(!t.claims.any(ms), "{:?}", t.claims);
    assert_eq!(t.transports().count(), 3);
    assert!(t.spoof.score() < REPORT_MIN, "{:?}", t.spoof.flags());
    assert_on_real_path(t, ms);
    assert!(drain(&mut tr, ms).is_empty());
}

/// Bluetooth 4: one message per advert, and the address rotates mid-flight.
/// Still one track, and the position keeps following the drone.
#[test]
fn rotating_bluetooth_address_stays_one_track() {
    let mut tr = hub();
    let mut ms = 0;
    let mut k = 0u64;
    while ms < 60_000 {
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        let mac = if ms < 30_000 { BLE_A } else { BLE_B };
        let m = match k % 3 {
            0 => loc(real_at(ms), ms),
            1 => basic(SN),
            _ => sys(at(0.0, 0.0)),
        };
        feed(&mut tr, 1, Transport::BleLegacy, mac, &[m], ms);
        if ms > 33_000 {
            assert_eq!(tr.len(), 1, "at {ms}: {:?}", tr.tracks());
            assert!(by_serial(&tr, SN).location.as_ref().unwrap().rx_ms + 1_000 >= ms, "position not following at {ms}");
        }
        k += 1;
        ms += 200;
    }
    let t = by_serial(&tr, SN);
    assert!(t.spoof.score() < REPORT_MIN, "{:?}", t.spoof.flags());
}

/// A drone that sends a serial and a session ID and changes the session ID
/// stays one track, and shows the new session ID within a few seconds; a
/// session-only drone that changes it is linked by continuity.
#[test]
fn session_id_change_is_followed() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 20_000 {
        let s = if ms < 10_000 { session(1) } else { session(2) };
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        feed(&mut tr, 0, Transport::WifiBeacon, MAC_A, &[basic(SN), s, loc(real_at(ms), ms)], ms);
        ms += 500;
    }
    assert_eq!(tr.len(), 1);
    let t = &tr.tracks()[0];
    let sess = t.uas_ids.iter().find(|u| u.id_type == IdType::SpecificSession).unwrap();
    assert_eq!(sess.id.as_bytes()[2], 2, "the new session ID is shown");
    assert!(t.spoof.score() < REPORT_MIN);

    // Session ID only: a new track, linked to the old one.
    let mut tr = hub();
    let mut ms = 0;
    while ms < 20_000 {
        let s = if ms < 10_000 { session(1) } else { session(2) };
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        feed(&mut tr, 0, Transport::WifiBeacon, MAC_A, &[s, loc(real_at(ms), ms)], ms);
        ms += 500;
    }
    assert_eq!(tr.len(), 2);
    let new = tr.tracks().iter().max_by_key(|t| t.first_seen_ms).unwrap();
    let old = tr.tracks().iter().min_by_key(|t| t.first_seen_ms).unwrap();
    assert_eq!(new.continuity.previous, Some(old.id));
}

/// The drone's own GPS glitches 1.5 km away for 4 s, then recovers: the
/// shown position never leaves the path and nothing is raised.
#[test]
fn gps_glitch_that_recovers_raises_nothing() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 60_000 {
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        let glitch = (20_000..24_000).contains(&ms);
        let p = if glitch { at(1_700.0, -900.0) } else { real_at(ms) };
        feed(&mut tr, 0, Transport::WifiBeacon, MAC_A, &[basic(SN), loc(p, ms), sys(at(0.0, 0.0))], ms);
        assert_on_real_path(by_serial(&tr, SN), ms);
        ms += 500;
    }
    let t = by_serial(&tr, SN);
    assert!(t.spoof.flags().iter().all(|f| f.confidence < REPORT_MIN), "{:?}", t.spoof.flags());
    assert!(t.spoof.flag(Check::PositionJump).is_none());
    assert!(!t.claims.ever_disputed());
    assert_eq!(t.claims.rejected(), 8, "the glitch's reports were held, not shown");
    assert!(drain(&mut tr, ms).is_empty());
    assert!(tr.take_dispute(ms).is_none());
}

/// Two nodes hear every frame.
#[test]
fn a_drone_heard_by_two_nodes_is_one_clean_track() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 60_000 {
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        let f = [basic(SN), loc(real_at(ms), ms), sys(at(0.0, 0.0))];
        feed(&mut tr, 0, Transport::WifiBeacon, MAC_A, &f, ms);
        feed(&mut tr, 1, Transport::WifiBeacon, MAC_A, &f, ms + 15);
        ms += 500;
    }
    assert_eq!(tr.len(), 1);
    let t = &tr.tracks()[0];
    assert!(t.spoof.flags().is_empty(), "{:?}", t.spoof.flags());
    assert!(drain(&mut tr, ms).is_empty());
}

// ----------------------------------------------------------- DJI kit --

fn rad(deg: f64) -> i32 {
    (deg.to_radians() * 1e7).round() as i32
}

fn serial16(sn: &str) -> [u8; 16] {
    let mut s = [0u8; 16];
    s[..sn.len()].copy_from_slice(sn.as_bytes());
    s
}

fn droneid(sn: &str, p: LatLon) -> DroneIdSighting {
    DroneIdSighting {
        freq: 24_445,
        rssi: -70,
        snr_db: 12,
        uptime_us: 0,
        record: DroneIdRecord::Flight {
            serial: serial16(sn),
            lat_raw: rad(p.lat),
            lon_raw: rad(p.lon),
            altitude_raw: 328,
            height_raw: 164,
            v_north: 0,
            v_east: 0,
            v_up: 0,
            yaw_raw: 0,
            pilot_lat_raw: 0,
            pilot_lon_raw: 0,
            home_lat_raw: 0,
            home_lon_raw: 0,
            product_type: 68,
        },
    }
}

fn dji_element(sn: &str, p: LatLon) -> heapless::Vec<u8, 255> {
    use rid::dji::{FlightInfo, FlightInfoExt};
    let info = FlightInfo {
        version: 2,
        seq: 7,
        state: FlightInfo::STATE_SERIAL_VALID | FlightInfo::STATE_PRIVACY_DISABLED,
        serial: FixedStr::new(sn.as_bytes()).unwrap(),
        position: Some(p),
        ext: Some(FlightInfoExt {
            altitude: 60,
            height: 45,
            v_north: 0,
            v_east: 100,
            v_up: 0,
            pitch: 0.0,
            roll: 0.0,
            yaw: 45.0,
            home: None,
            product_type: 21,
            uuid: FixedStr::default(),
        }),
    };
    let mut body = [0u8; 200];
    let n = rid::encode::dji_flight_info(&info, &mut body).unwrap();
    heapless::Vec::from_slice(&body[..n]).unwrap()
}

fn o4(kind: link::O4Kind, key: [u8; 4]) -> DroneIdSighting {
    DroneIdSighting {
        freq: 24_445,
        rssi: -70,
        snr_db: 12,
        uptime_us: 0,
        record: DroneIdRecord::O4 { kind, key_id: key, object: [0x5a; 135].into_iter().collect() },
    }
}

fn infp10(sn: &str, p: LatLon, seq: u16) -> droneid::o4plain::Infp10 {
    use droneid::o4plain::Infp10;
    Infp10 {
        version: 3,
        seq,
        state: Infp10::STATE_SERIAL_VALID
            | Infp10::STATE_MOTORS_ON
            | Infp10::STATE_IN_AIR
            | Infp10::STATE_POSITION_VALID
            | Infp10::STATE_ALTITUDE_VALID
            | Infp10::STATE_HEIGHT_VALID,
        serial: serial16(sn),
        lon_raw: rad(p.lon),
        lat_raw: rad(p.lat),
        altitude_m: 100,
        height_dm: 500,
        v_north: 0,
        v_east: 0,
        v_up: 0,
        heading_cdeg: 0,
        gps_time_ms: 1_794_232_987_250 + u64::from(seq) * 200,
        app_lat_raw: 0,
        app_lon_raw: 0,
        home_lon_raw: droneid::o4plain::HOME_UNSET,
        home_lat_raw: droneid::o4plain::HOME_UNSET,
        product_type: 84,
        identity_len: 0,
        identity: [0; 20],
        account_len: 0,
        account: [0; 20],
    }
}

/// A quiet site: five honest drones for two minutes. No dispute and no
/// spoof event.
#[test]
fn quiet_honest_site_raises_nothing() {
    let mut tr = hub();
    let mut ms = 0;
    while ms < 120_000 {
        tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(40.0), ms);
        for n in 0..5u8 {
            let sn = std::format!("1ZZZ9HONEST{n:03}");
            let p = at(300.0 * f64::from(n) + 4.0 * ms as f64 / 1000.0, 200.0 * f64::from(n));
            feed(
                &mut tr,
                n % 2,
                Transport::WifiBeacon,
                [0x02, 0, 0, 0, 1, n],
                &[basic(&sn), loc(p, ms), sys(at(0.0, -50.0 * f64::from(n))), self_id("honest")],
                ms + u64::from(n) * 37,
            );
        }
        ms += 1_000;
    }
    assert_eq!(tr.len(), 5);
    for t in tr.tracks() {
        assert!(t.spoof.score() < REPORT_MIN, "{:?}", t.spoof.flags());
        assert!(!t.claims.any(ms), "{:?}", t.claims);
    }
    assert!(drain(&mut tr, ms).is_empty());
    assert!(tr.take_dispute(ms).is_none());
    assert_eq!(tr.disputed_count(ms), 0);
}
