//! Aircraft table tests: trust rules, CPR pairing, plausibility, output.

extern crate std;

use std::vec::Vec;

use crate::alt;
use crate::cpr::{self, Kind as CprKind};
use crate::frame::Frame;
use crate::input::Received;
use crate::table::{Config, Table, distance_m};
use crate::tests::{airborne_me, ap_frame, df4_head, es_frame, put};

fn rx(frame: Frame) -> Received {
    Received { frame, signal_dbm: Some(-70), ticks_12mhz: None }
}

const ICAO: u32 = 0xA1B2C3;
const PHL: (f64, f64) = (39.9526, -75.1652);

fn ident(icao: u32, call: &str) -> Frame {
    let mut m = [0u8; 7];
    put(&mut m, 1, 5, 4);
    put(&mut m, 6, 3, 1);
    for (i, c) in call.bytes().chain(core::iter::repeat(b' ')).take(8).enumerate() {
        let v = match c {
            b'A'..=b'Z' => u32::from(c - b'A' + 1),
            b'0'..=b'9' => u32::from(c),
            _ => 32,
        };
        put(&mut m, 9 + 6 * i, 6, v);
    }
    es_frame(17, 5, icao, m)
}

fn pos(icao: u32, lat: f64, lon: f64, alt_ft: i32, odd: bool) -> Frame {
    es_frame(17, 5, icao, airborne_me(11, alt_ft, lat, lon, odd))
}

fn report<const N: usize>(t: &mut Table<N>, now: u64) -> Vec<adsb::Aircraft> {
    let mut v = Vec::new();
    t.report(now, |a| v.push(a));
    v
}

#[test]
fn global_pair_gives_a_position_and_a_full_record() {
    let mut t: Table<8> = Table::default();
    let (lat, lon) = (40.05, -75.01);
    t.ingest(&rx(ident(ICAO, "N123AB")), 1000).unwrap();
    t.ingest(&rx(pos(ICAO, lat, lon, 2500, false)), 1200).unwrap();
    assert!(t.get(ICAO).unwrap().position().is_none(), "no reference, no pair yet");
    t.ingest(&rx(pos(ICAO, lat, lon, 2500, true)), 1700).unwrap();
    let p = t.get(ICAO).unwrap().position().unwrap();
    assert!(distance_m(p, (lat, lon)) < 10.0);
    assert_eq!(t.stats.positions_global, 1);

    let out = report(&mut t, 2000);
    assert_eq!(out.len(), 1);
    let a = out[0];
    assert_eq!(a.icao24(), ICAO);
    assert_eq!(a.callsign.as_str(), "N123AB");
    assert_eq!(a.kind(), adsb::Kind::Light);
    assert_eq!(a.baro_altitude_ft(), Some(2500));
    assert!(a.position().is_some());
    assert!(a.position_updated());
    assert!(a.airborne());
    assert!(a.heard());
    assert_eq!(a.es_frames_per_s, 3);
    assert_eq!(a.signal_dbm, -70);
    // The record survives the CSBee round trip the web-head feed uses.
    let mut line = [0u8; adsb::MAX_LINE];
    let n = adsb::encode::aircraft(&a, &mut line).unwrap();
    let adsb::Report::Aircraft(b) = adsb::parse(&line[..n]).unwrap() else { panic!() };
    assert_eq!(b.icao24(), ICAO);
    assert_eq!(b.callsign.as_str(), "N123AB");
    // Next interval: nothing new heard, so the updated bits are clear.
    let out = report(&mut t, 3000);
    assert!(!out[0].position_updated());
    assert!(!out[0].heard());
}

#[test]
fn local_from_receiver_and_from_own_position() {
    let mut t: Table<8> = Table::default();
    t.set_reference(Some(PHL));
    t.ingest(&rx(pos(ICAO, 39.87, -75.24, 1500, true)), 0).unwrap();
    let p = t.get(ICAO).unwrap().position().unwrap();
    assert!(distance_m(p, (39.87, -75.24)) < 10.0);
    assert_eq!(t.stats.positions_local, 1);
    // Without the receiver's position or a pair: nothing.
    let mut t2: Table<8> = Table::new(Config { local_from_receiver: false, ..Config::default() });
    t2.set_reference(Some(PHL));
    t2.ingest(&rx(pos(ICAO, 39.87, -75.24, 1500, true)), 0).unwrap();
    assert!(t2.get(ICAO).unwrap().position().is_none());
}

#[test]
fn pair_too_far_apart_in_time_is_not_used() {
    let mut t: Table<8> = Table::default();
    t.ingest(&rx(pos(ICAO, 40.0, -75.0, 3000, false)), 0).unwrap();
    t.ingest(&rx(pos(ICAO, 40.0, -75.0, 3000, true)), 11_000).unwrap();
    assert!(t.get(ICAO).unwrap().position().is_none());
}

#[test]
fn address_parity_only_for_known_aircraft() {
    let mut t: Table<8> = Table::default();
    let ac = {
        let f12 = alt::ac12_field_25ft(4000).unwrap();
        ((f12 & 0x0FC0) << 1) | (f12 & 0x003F)
    };
    let df4 = ap_frame(&df4_head(4, ac, None), ICAO);
    assert!(t.ingest(&rx(df4), 0).is_err());
    assert_eq!(t.stats.unconfirmed, 1);
    assert!(t.is_empty());
    t.ingest(&rx(ident(ICAO, "TEST1")), 100).unwrap();
    t.ingest(&rx(df4), 200).unwrap();
    assert_eq!(t.get(ICAO).unwrap().baro_altitude_ft(), Some(4000));
    // A minute after the last verified message it is no longer trusted.
    assert!(t.ingest(&rx(df4), 200 + 61_000).is_err());
}

#[test]
fn two_bit_repairs_only_for_known_aircraft() {
    let mut t: Table<8> = Table::default();
    let mut bad = ident(ICAO, "TEST2");
    bad.flip(40);
    bad.flip(77);
    assert!(t.ingest(&rx(bad), 0).is_err());
    assert_eq!(t.stats.fix_2_refused, 1);
    assert!(t.is_empty());
    t.ingest(&rx(ident(ICAO, "TEST2")), 100).unwrap();
    t.ingest(&rx(bad), 200).unwrap();
    assert_eq!(t.stats.fixed_2, 1);
    // One-bit repairs may create an aircraft.
    let mut one = ident(0xA00002, "TEST3");
    one.flip(50);
    t.ingest(&rx(one), 300).unwrap();
    assert_eq!(t.stats.fixed_1, 1);
    assert!(t.get(0xA00002).is_some());
}

#[test]
fn implausible_jump_is_refused() {
    let mut t: Table<8> = Table::default();
    t.ingest(&rx(pos(ICAO, 40.0, -75.0, 3000, false)), 0).unwrap();
    t.ingest(&rx(pos(ICAO, 40.0, -75.0, 3000, true)), 500).unwrap();
    assert!(t.get(ICAO).unwrap().position().is_some());
    // 1 s later, 100 km away: refused.
    t.ingest(&rx(pos(ICAO, 40.9, -75.0, 3000, false)), 1500).unwrap();
    t.ingest(&rx(pos(ICAO, 40.9, -75.0, 3000, true)), 1600).unwrap();
    let p = t.get(ICAO).unwrap().position().unwrap();
    assert!(distance_m(p, (40.0, -75.0)) < 10.0);
    assert!(t.stats.positions_refused >= 1);
}

#[test]
fn range_limit_from_receiver() {
    let mut t: Table<8> = Table::default();
    t.set_reference(Some(PHL));
    // 700 km away, as a global pair: refused by the 500 km limit.
    t.ingest(&rx(pos(ICAO, 45.5, -73.6, 30000, false)), 0).unwrap();
    t.ingest(&rx(pos(ICAO, 45.5, -73.6, 30000, true)), 500).unwrap();
    assert!(t.get(ICAO).unwrap().position().is_none());
    assert!(t.stats.positions_refused >= 1);
}

#[test]
fn surface_positions_use_the_reference() {
    let mut t: Table<8> = Table::default();
    t.set_reference(Some(PHL));
    let mk = |odd: bool| {
        let mut m = [0u8; 7];
        put(&mut m, 1, 5, 7);
        put(&mut m, 6, 7, 20); // 5.5 kt
        put(&mut m, 13, 1, 1);
        put(&mut m, 14, 7, 32); // 90 degrees
        let c = cpr::encode(39.8744, -75.2424, odd, CprKind::Surface);
        put(&mut m, 22, 1, u32::from(odd));
        put(&mut m, 23, 17, c.lat);
        put(&mut m, 40, 17, c.lon);
        es_frame(17, 4, ICAO, m)
    };
    t.ingest(&rx(mk(false)), 0).unwrap();
    t.ingest(&rx(mk(true)), 1000).unwrap();
    let p = t.get(ICAO).unwrap().position().unwrap();
    assert!(distance_m(p, (39.8744, -75.2424)) < 5.0, "{p:?}");
    let a = report(&mut t, 1500)[0];
    assert!(!a.airborne());
    assert_eq!(a.speed_kt(), Some(6));
    assert_eq!(a.direction_deg(), Some(90.0));
}

#[test]
fn tis_b_and_ads_r_are_counted() {
    let mut t: Table<8> = Table::default();
    t.ingest(&rx(es_frame(18, 2, 0xA00010, airborne_me(11, 2000, 40.0, -75.0, false))), 0).unwrap();
    t.ingest(&rx(es_frame(18, 6, 0xA00011, airborne_me(11, 2000, 40.0, -75.0, false))), 0).unwrap();
    assert_eq!(t.stats.tis_b, 1);
    assert_eq!(t.stats.ads_r, 1);
    assert!(t.get(0xA00010).unwrap().relayed_only);
}

#[test]
fn table_full_evicts_only_quiet_aircraft() {
    let mut t: Table<2> = Table::default();
    t.ingest(&rx(ident(0xA00001, "A")), 0).unwrap();
    t.ingest(&rx(ident(0xA00002, "B")), 0).unwrap();
    t.ingest(&rx(ident(0xA00003, "C")), 1000).unwrap();
    assert!(t.get(0xA00003).is_none(), "both others heard in the last 5 s");
    t.ingest(&rx(ident(0xA00002, "B")), 5500).unwrap();
    t.ingest(&rx(ident(0xA00003, "C")), 6000).unwrap();
    assert!(t.get(0xA00003).is_some());
    assert!(t.get(0xA00001).is_none());
    // Expiry.
    let out = report(&mut t, 6000 + 61_000);
    assert!(out.is_empty());
    assert!(t.is_empty());
}

#[test]
fn stats_line_counts_the_interval() {
    let mut t: Table<8> = Table::default();
    t.ingest(&rx(ident(ICAO, "X")), 0).unwrap();
    let _ = t.ingest(&rx(Frame::from_hex("8D4840D6202CC371C32CE0576099").unwrap()), 0);
    let s = t.report(1000, |_| {});
    assert_eq!(s.raw_extended_squitters_per_s, 2);
    assert_eq!(s.extended_squitters_per_s, 2, "the second was repaired");
    assert_eq!(s.aircraft, 2);
    let s = t.report(2000, |_| {});
    assert_eq!(s.raw_extended_squitters_per_s, 0);
}

#[test]
fn global_pair_overrides_a_receiver_local_alias() {
    // An aircraft 620 km north of the receiver (beyond half a CPR zone):
    // one message decoded against the receiver aliases to a point near it.
    // The first even/odd pair moves it to the right place.
    let mut t: Table<8> = Table::new(Config { max_range_km: 1000.0, ..Config::default() });
    t.set_reference(Some(PHL));
    t.ingest(&rx(pos(ICAO, 45.5, -73.6, 39000, false)), 0).unwrap();
    let alias = t.get(ICAO).unwrap().position().unwrap();
    assert!(distance_m(alias, PHL) < 250_000.0, "{alias:?}");
    t.ingest(&rx(pos(ICAO, 45.5, -73.6, 39000, true)), 500).unwrap();
    let p = t.get(ICAO).unwrap().position().unwrap();
    assert!(distance_m(p, (45.5, -73.6)) < 10.0, "{p:?}");
}

/// `init_in_place` over garbage gives the same empty table as `new`, and
/// it works like one.
#[test]
fn init_in_place_builds_an_empty_table() {
    let mut slot = std::boxed::Box::new(core::mem::MaybeUninit::<Table<8>>::uninit());
    // Garbage first, as reused memory would hold.
    // SAFETY: writing bytes into a MaybeUninit is always allowed.
    unsafe { core::ptr::write_bytes(slot.as_mut_ptr().cast::<u8>(), 0xA5, core::mem::size_of::<Table<8>>()) };
    // SAFETY: the box is writable and aligned for Table<8>.
    unsafe { Table::init_in_place(slot.as_mut_ptr(), Config::default()) };
    // SAFETY: every field was written by init_in_place.
    let t = unsafe { slot.assume_init_mut() };
    assert_eq!(t.len(), 0);
    assert_eq!(t.stats, Table::<8>::default().stats);
    let (lat, lon) = (40.05, -75.01);
    t.ingest(&rx(ident(ICAO, "N123AB")), 1000).unwrap();
    t.ingest(&rx(pos(ICAO, lat, lon, 2500, false)), 1200).unwrap();
    t.ingest(&rx(pos(ICAO, lat, lon, 2500, true)), 1700).unwrap();
    assert!(distance_m(t.get(ICAO).unwrap().position().unwrap(), (lat, lon)) < 10.0);
    assert_eq!(report(t, 2000).len(), 1);
}

/// An operational status (TC 31 airborne): version 2, NIC_A, NACp, SIL 3.
fn op_status(icao: u32, nic_a: bool, nacp: u8) -> Frame {
    let mut m = [0u8; 7];
    put(&mut m, 1, 5, 31);
    put(&mut m, 41, 3, 2);
    put(&mut m, 44, 1, u32::from(nic_a));
    put(&mut m, 45, 4, u32::from(nacp));
    put(&mut m, 51, 2, 3);
    es_frame(17, 5, icao, m)
}

#[test]
fn nicnac_from_position_type_code_and_operational_status() {
    let mut t: Table<8> = Table::default();
    let (lat, lon) = (40.05, -75.01);
    // Nothing yet: 0, as the ADSBee prints before it has heard anything.
    t.ingest(&rx(ident(ICAO, "N1")), 900).unwrap();
    assert_eq!(report(&mut t, 950)[0].nicnac, 0);
    t.ingest(&rx(op_status(ICAO, false, 10)), 1000).unwrap();
    // TC 11 with NIC_A = NIC_B = 0: NIC 8 (< 0.1 NM).
    t.ingest(&rx(pos(ICAO, lat, lon, 2500, false)), 1200).unwrap();
    let a = report(&mut t, 1500)[0];
    assert_eq!((a.nic(), a.nacp(), adsb::nicnac::sil(a.nicnac)), (8, 10, 3));
    // A jammed receiver: TC 18 (NIC 0) and NACp 0 from then on.
    t.ingest(&rx(es_frame(17, 5, ICAO, airborne_me(18, 2500, lat, lon, true))), 2000).unwrap();
    t.ingest(&rx(op_status(ICAO, false, 0)), 2100).unwrap();
    let a = report(&mut t, 2500)[0];
    assert_eq!((a.nic(), a.nacp()), (0, 0));
    // TC 9: NIC 11.
    t.ingest(&rx(es_frame(17, 5, ICAO, airborne_me(9, 2500, lat, lon, false))), 3000).unwrap();
    assert_eq!(report(&mut t, 3500)[0].nic(), 11);
    // Old values lapse: 0 again after VALID_MS.
    t.ingest(&rx(ident(ICAO, "N1")), 3000 + crate::table::VALID_MS + 1).unwrap();
    assert_eq!(report(&mut t, 3000 + crate::table::VALID_MS + 2)[0].nicnac, 0);
}
