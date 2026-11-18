//! Sound contacts (`sound`).

extern crate std;

use crate::sound::*;
use crate::tests::{CITY_HALL, WIFI_MAC, det, location, serial};
use crate::tests_conflict::heli;
use crate::*;
use rid::Transport;

fn heard(pitch: u16) -> SoundReport {
    SoundReport { heard: true, pitch_hz: pitch, confidence: 70, level_dbfs: -40, small_props: pitch >= 350 }
}

const QUIET: SoundReport =
    SoundReport { heard: false, pitch_hz: 0, confidence: 0, level_dbfs: -60, small_props: false };

/// Hear `pitch` once a second from `from_s` to `to_s`.
fn listen(tr: &mut Tracker, from_s: u64, to_s: u64, pitch: u16) -> Option<SoundId> {
    let mut id = None;
    for s in from_s..=to_s {
        id = tr.ingest_sound(&heard(pitch), s * 1000);
    }
    id
}

#[test]
fn sound_alone_alerts_once_after_five_seconds_as_no_radio() {
    let mut tr: Tracker = Tracker::default();
    assert_eq!(tr.ingest_sound(&QUIET, 0), None);
    let id = listen(&mut tr, 1, 4, 190).unwrap();
    assert_eq!(tr.take_new_sound(4_000), None, "too short to alert");
    listen(&mut tr, 5, 8, 195);
    assert_eq!(tr.take_new_sound(8_000), Some(id));
    assert_eq!(tr.take_new_sound(8_500), None, "once only");
    let c = tr.get_sound(id).unwrap();
    assert_eq!(c.kind, SoundKind::NoRadio);
    assert_eq!((c.pitch_min_hz, c.pitch_max_hz, c.reports), (190, 195, 8));
    assert_eq!(c.size_label(), "Large propellers");
    assert_eq!(tr.sounds().len(), 1);
}

#[test]
fn a_pause_under_five_seconds_is_one_contact_and_a_longer_one_is_two() {
    let mut tr: Tracker = Tracker::default();
    let a = listen(&mut tr, 0, 3, 600).unwrap();
    let b = listen(&mut tr, 7, 9, 610).unwrap();
    assert_eq!(a, b);
    let c = listen(&mut tr, 20, 22, 610).unwrap();
    assert_ne!(a, c);
    assert_eq!(tr.sounds().len(), 2);
    assert!(tr.active_sound(22_500).is_some());
    assert!(tr.active_sound(40_000).is_none());
}

#[test]
fn a_radio_drone_makes_it_with_radio() {
    let mut tr: Tracker = Tracker::default();
    let d = det(Transport::WifiBeacon, WIFI_MAC, -60, &[serial("SN-1"), location(CITY_HALL.0, CITY_HALL.1, 60.0)]);
    tr.ingest(0, &d, 1_000);
    let id = listen(&mut tr, 2, 8, 200).unwrap();
    assert_eq!(tr.get_sound(id).unwrap().kind, SoundKind::WithRadio);
    assert_eq!(tr.take_new_sound(8_000), Some(id));
    // Long after the radio went quiet, a new contact is on its own again.
    let later = listen(&mut tr, 100, 106, 200).unwrap();
    assert_eq!(tr.get_sound(later).unwrap().kind, SoundKind::NoRadio);
}

#[test]
fn a_radio_drone_arriving_mid_contact_upgrades_it_and_it_never_downgrades() {
    let mut tr: Tracker = Tracker::default();
    let id = listen(&mut tr, 0, 3, 200).unwrap();
    assert_eq!(tr.get_sound(id).unwrap().kind, SoundKind::NoRadio);
    let d = det(Transport::WifiBeacon, WIFI_MAC, -60, &[serial("SN-1")]);
    tr.ingest(0, &d, 3_500);
    listen(&mut tr, 4, 30, 200);
    assert_eq!(tr.get_sound(id).unwrap().kind, SoundKind::WithRadio, "radio 26 s ago, same contact");
}

#[test]
fn a_tracked_helicopter_overhead_explains_the_sound_and_does_not_alert() {
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(10.0), 0);
    // 800 m north, about 300 m up.
    tr.ingest_aircraft(&heli(0xA1B2C3, 800.0, 1_000, 1_000, 7, 180.0, 80), 0);
    let id = listen(&mut tr, 1, 10, 110).unwrap();
    let c = *tr.get_sound(id).unwrap();
    assert_eq!(c.kind, SoundKind::Aircraft);
    assert_eq!(c.aircraft.unwrap().icao, 0xA1B2C3);
    assert!((800..900).contains(&c.aircraft_range_m), "slant {}", c.aircraft_range_m);
    assert_eq!(tr.take_new_sound(10_000), None);

    let mut buf = [0u8; MAX_SOUND_LINE];
    let n = sound_line(&c, &tr.context(10_000, None), &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["t"], "sound");
    assert_eq!(v["kind"], "Aircraft");
    assert_eq!(v["aircraft"], "A1B2C3");
    assert_eq!(v["alerts"], false);
    assert_eq!(v["pitch_hz"], 110);
}

#[test]
fn a_distant_or_landed_aircraft_explains_nothing() {
    let mut tr: Tracker = Tracker::default();
    tr.set_own_position(CITY_HALL.0, CITY_HALL.1, Some(10.0), 0);
    tr.ingest_aircraft(&heli(0x000001, 9_000.0, 1_000, 1_000, 7, 180.0, 80), 0);
    let id = listen(&mut tr, 1, 8, 110).unwrap();
    assert_eq!(tr.get_sound(id).unwrap().kind, SoundKind::NoRadio);
    assert_eq!(tr.take_new_sound(8_000), Some(id));
}

#[test]
fn without_own_position_no_aircraft_is_assumed_audible() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_aircraft(&heli(0x000001, 500.0, 1_000, 1_000, 7, 180.0, 80), 0);
    assert_eq!(tr.audible_aircraft(1_000), None);
}

#[test]
fn the_table_keeps_the_latest_sixteen_and_the_line_fits() {
    let mut tr: Tracker = Tracker::default();
    for i in 0..20u64 {
        listen(&mut tr, i * 10, i * 10 + 1, 1_599);
    }
    assert_eq!(tr.sounds().len(), MAX_CONTACTS);
    assert!(tr.sounds().iter().all(|c| c.first_seen_ms >= 40_000));
    let mut worst = *tr.sounds().last().unwrap();
    worst.aircraft = Some(crate::aircraft::AircraftKey { icao: 0xFFFFFF, source: adsb::Source::ModeS });
    worst.aircraft_range_m = u32::MAX;
    worst.clip = u16::MAX;
    worst.reports = u32::MAX;
    worst.first_seen_ms = u64::MAX;
    worst.last_seen_ms = u64::MAX;
    worst.pitch_max_hz = u16::MAX;
    let mut buf = [0u8; MAX_SOUND_LINE];
    let ctx = tr.context(u64::MAX, None);
    assert!(sound_line(&worst, &ctx, &mut buf).is_ok());
}
