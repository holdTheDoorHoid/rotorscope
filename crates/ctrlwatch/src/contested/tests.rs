use super::*;
use crate::elrs::testenc::Tx;
use crate::elrs::{ElrsRate, OtaVersion, lora_rx, sync_freq, uid_from_phrase};

type W = Watch<8>;

fn meta(t_us: u64, rssi: i16) -> RxMeta {
    RxMeta {
        rssi_dbm: rssi,
        snr_db: 8,
        t_us,
    }
}

fn reports(w: &mut W, now: u64, cfg: &Config) -> std::vec::Vec<Report> {
    let mut v = std::vec::Vec::new();
    w.take_due(now, cfg, |r| v.push(*r));
    v
}

/// Small deterministic noise in -range..=range.
struct Lcg(u64);
impl Lcg {
    fn jitter(&mut self, range: i64) -> i64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as i64 % (2 * range + 1)) - range
    }
}

// ------------------------------------------------------------ ExpressLRS --

const RATE: ElrsRate = ElrsRate::L900_200; // 5 ms interval, rate enum 5

fn elrs_rx(rate: ElrsRate) -> RxConfig {
    lora_rx(rate, sync_freq(Region::Fcc915, OtaVersion::V4), false, Region::Fcc915, OtaVersion::V4).unwrap()
}

fn tx(phrase: &str, model_id: Option<u8>) -> Tx {
    Tx {
        uid: uid_from_phrase(phrase),
        version: OtaVersion::V4,
        model_id,
    }
}

/// A transmitter whose nonce was 0 at `origin_us` and which runs `ppm` fast.
struct Elrs {
    tx: Tx,
    origin_us: u64,
    ppm: i64,
    tlm: u8,
    rate: ElrsRate,
}

impl Elrs {
    fn new(tx: Tx, origin_us: u64) -> Self {
        Self {
            tx,
            origin_us,
            ppm: 0,
            tlm: 2,
            rate: RATE,
        }
    }

    /// The sync packet this transmitter sends nearest to `t_us`, and when.
    fn sync_near(&self, t_us: u64) -> (std::vec::Vec<u8>, u64) {
        let interval = self.rate.info().interval_us as i64;
        let interval = interval + interval * self.ppm / 1_000_000;
        let k = (t_us as i64 - self.origin_us as i64).div_euclid(interval);
        let at = self.origin_us as i64 + k * interval;
        let nonce = k.rem_euclid(256) as u8;
        let rate_field = self.rate.info().v4_enum;
        (self.tx.sync(false, 40, nonce, rate_field, 0, self.tlm), at as u64)
    }
}

fn feed_elrs(w: &mut W, e: &Elrs, t: u64, jitter: i64, rssi: i16, cfg: &Config) {
    let (p, at) = e.sync_near(t);
    let stamped = (at as i64 + jitter) as u64;
    w.observe(&p, &elrs_rx(e.rate), meta(stamped, rssi), cfg);
}

#[test]
fn elrs_second_transmitter_on_one_uid_fires() {
    let cfg = Config::default();
    let mut w = W::new();
    let a = Elrs::new(tx("victim", None), 0);
    let b = Elrs::new(tx("victim", None), 777_777); // same UID, own clock
    let mut out = std::vec::Vec::new();
    for i in 0..8u64 {
        let t = 1_000_000 + i * 2_000_000;
        feed_elrs(&mut w, &a, t, 0, -60, &cfg);
        feed_elrs(&mut w, &b, t + 1_000_000, 0, -75, &cfg);
        out.extend(reports(&mut w, t + 1_500_000, &cfg));
    }
    let r = out.last().expect("contested");
    assert_eq!(r.system, System::ExpressLrs);
    assert_eq!(r.transmitters, 2);
    assert_eq!(r.rules, RULE_PHASE);
    assert!(r.events >= 2);
    assert_eq!(r.id, None, "ids stay in the node without the opt-in");
    let d = decode_detail(r.detail());
    assert_eq!((d.system, d.transmitters, d.rules, d.region), (Some(System::ExpressLrs), 2, RULE_PHASE, Some(Region::Fcc915)));
    // With the opt-in, the UID goes out.
    let ids = Config {
        read_ids: true,
        ..Default::default()
    };
    feed_elrs(&mut w, &a, 40_000_000, 0, -60, &ids);
    let r = reports(&mut w, 40_000_001, &ids);
    assert!(matches!(r[0].id, Some(LinkId::ElrsUid { .. })));
}

#[test]
fn elrs_needs_alternation_not_one_switch() {
    // A A A B B B: one change of transmitter is a restart, not a contest.
    let cfg = Config::default();
    let mut w = W::new();
    let a = Elrs::new(tx("victim", None), 0);
    let b = Elrs::new(tx("victim", None), 333_333);
    for i in 0..4u64 {
        feed_elrs(&mut w, &a, 1_000_000 + i * 2_000_000, 0, -60, &cfg);
    }
    for i in 0..4u64 {
        feed_elrs(&mut w, &b, 20_000_000 + i * 2_000_000, 0, -62, &cfg);
    }
    assert!(reports(&mut w, 40_000_000, &cfg).is_empty());
}

#[test]
fn elrs_one_transmitter_jitter_drift_fast_drone_and_loss_is_quiet() {
    let cfg = Config::default();
    let mut w = W::new();
    let mut a = Elrs::new(tx("pilot", None), 12_345);
    a.ppm = 30;
    let mut rng = Lcg(7);
    let mut t = 500_000u64;
    for i in 0..60u64 {
        // Irregular gaps (lost syncs, the node elsewhere), up to minutes.
        t += match i % 7 {
            0 => 180_000_000,
            1 | 2 => 5_000_000,
            3 => 400_000,
            _ => 23_000_000,
        };
        // Signal strength swinging 40 dB packet to packet.
        let rssi = if i % 2 == 0 { -45 } else { -95 };
        feed_elrs(&mut w, &a, t, rng.jitter(1_500), rssi, &cfg);
        assert!(reports(&mut w, t + 1, &cfg).is_empty(), "packet {i}");
    }
}

#[test]
fn elrs_separate_links_are_not_contested() {
    // Different binding phrases, and one phrase with Model Match on two
    // model IDs: separate links by design, interleaved in time.
    let cfg = Config::default();
    let mut w = W::new();
    let txs = [
        Elrs::new(tx("one", None), 0),
        Elrs::new(tx("two", None), 400_000),
        Elrs::new(tx("club", Some(1)), 800_000),
        Elrs::new(tx("club", Some(2)), 1_100_000),
    ];
    for i in 0..10u64 {
        for (k, e) in txs.iter().enumerate() {
            feed_elrs(&mut w, e, 1_000_000 + i * 3_000_000 + k as u64 * 500_000, 0, -70, &cfg);
        }
    }
    assert_eq!(w.len(), 4);
    assert!(reports(&mut w, 60_000_000, &cfg).is_empty());
}

#[test]
fn elrs_power_cycle_and_rate_change_are_quiet() {
    let cfg = Config::default();
    let mut w = W::new();
    // Power cycle: 8 s off, back with a new phase, same settings, same place.
    let a = Elrs::new(tx("pilot", None), 0);
    let a2 = Elrs::new(tx("pilot", None), 640_000);
    for i in 0..5u64 {
        feed_elrs(&mut w, &a, 1_000_000 + i * 1_000_000, 0, -60, &cfg);
    }
    for i in 0..5u64 {
        feed_elrs(&mut w, &a2, 14_000_000 + i * 1_000_000, 0, -63, &cfg);
    }
    // Then a packet-rate change (100 Hz): another cycle length entirely.
    let mut a3 = Elrs::new(tx("pilot", None), 50_000);
    a3.rate = ElrsRate::L900_100;
    for i in 0..5u64 {
        feed_elrs(&mut w, &a3, 25_000_000 + i * 1_000_000, 0, -61, &cfg);
    }
    assert!(reports(&mut w, 40_000_000, &cfg).is_empty());
}

#[test]
fn elrs_takeover_after_silence_is_a_handover() {
    let cfg = Config::default();
    let mut w = W::new();
    let a = Elrs::new(tx("victim", None), 0);
    let mut b = Elrs::new(tx("victim", None), 900_000);
    b.tlm = 5; // a different telemetry ratio from the attacker's radio
    for i in 0..5u64 {
        feed_elrs(&mut w, &a, 1_000_000 + i * 1_000_000, 0, -60, &cfg);
    }
    // 12 s silent (jammed), then a new transmitter.
    for i in 0..3u64 {
        feed_elrs(&mut w, &b, 17_000_000 + i * 1_000_000, 0, -61, &cfg);
    }
    let r = reports(&mut w, 20_000_000, &cfg);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].rules, RULE_HANDOVER);
    assert_eq!(r[0].transmitters, 2);
    // Reported once; more packets from the new one add nothing new.
    for i in 3..8u64 {
        feed_elrs(&mut w, &b, 17_000_000 + i * 1_000_000, 0, -61, &cfg);
    }
    assert_eq!(w.links[0].finding.unwrap().events, 1);

    // Same settings but 25 dB weaker: also a handover.
    let mut w = W::new();
    let b = Elrs::new(tx("victim", None), 900_000);
    for i in 0..5u64 {
        feed_elrs(&mut w, &a, 1_000_000 + i * 1_000_000, 0, -60, &cfg);
    }
    for i in 0..3u64 {
        feed_elrs(&mut w, &b, 17_000_000 + i * 1_000_000, 0, -85, &cfg);
    }
    assert_eq!(reports(&mut w, 20_000_000, &cfg)[0].rules, RULE_HANDOVER);

    // ...but not after a long silence (more than HANDOVER_GAP_US).
    let mut w = W::new();
    for i in 0..5u64 {
        feed_elrs(&mut w, &a, 1_000_000 + i * 1_000_000, 0, -60, &cfg);
    }
    for i in 0..3u64 {
        feed_elrs(&mut w, &b, 60_000_000 + i * 1_000_000, 0, -85, &cfg);
    }
    assert!(reports(&mut w, 70_000_000, &cfg).is_empty());
}

#[test]
fn elrs_reports_hold_then_lapse() {
    let cfg = Config::default();
    let mut w = W::new();
    let a = Elrs::new(tx("victim", None), 0);
    let b = Elrs::new(tx("victim", None), 500_000);
    let mut t = 0;
    for i in 0..6u64 {
        t = 1_000_000 + i * 2_000_000;
        feed_elrs(&mut w, &a, t, 0, -60, &cfg);
        feed_elrs(&mut w, &b, t + 1_000_000, 0, -70, &cfg);
    }
    assert_eq!(reports(&mut w, t + 1_100_000, &cfg).len(), 1);
    // Nothing new: refreshed every REFRESH_US while it stands...
    assert!(reports(&mut w, t + 3_000_000, &cfg).is_empty());
    assert_eq!(reports(&mut w, t + 1_100_000 + REFRESH_US, &cfg).len(), 1);
    // ...and silent after HOLD_US.
    assert!(reports(&mut w, t + HOLD_US + 2_000_000, &cfg).is_empty());
    w.expire(t + LINK_MAX_AGE_US + HOLD_US);
    assert!(w.is_empty());
}

// ----------------------------------------------------------------- mLRS --

fn mlrs_rx() -> RxConfig {
    crate::mlrs::lora_rx(crate::mlrs::MlrsMode::L900_31, 915_000_000, Region::Fcc915).unwrap()
}

/// Visits to one channel: every 43 frames of 32 ms.
const VISIT: u64 = 43 * 32_000;

#[test]
fn mlrs_second_uplink_transmitter_fires() {
    use crate::mlrs::testenc::{rx_frame, tx_frame};
    let cfg = Config::default();
    let mut w = W::new();
    let up = tx_frame(0x5A5A, 1, [1024; 8], &[]);
    let down = rx_frame(0x5A5A, 1, &[]);
    let mut out = std::vec::Vec::new();
    for v in 0..8u64 {
        let base = 10_000_000 + v * VISIT;
        // The real pair: uplink at phase 3 ms, the receiver's answer at 19 ms.
        w.observe(&up, &mlrs_rx(), meta(base + 3_000, -70), &cfg);
        w.observe(&down, &mlrs_rx(), meta(base + 19_000, -80), &cfg);
        // The intruder's uplink on its own frame clock (phase 11 ms),
        // reaching this channel on another frame.
        w.observe(&up, &mlrs_rx(), meta(base + 700_000 + 11_000, -65), &cfg);
        out.extend(reports(&mut w, base + 800_000, &cfg));
    }
    let r = out.last().expect("contested");
    assert_eq!(r.system, System::Mlrs);
    assert_eq!(r.rules, RULE_PHASE);
    assert_eq!(r.transmitters, 3, "two transmitters and the receiver");
}

#[test]
fn mlrs_one_link_over_scan_visits_is_quiet() {
    use crate::mlrs::testenc::{rx_frame, tx_frame};
    let cfg = Config::default();
    let mut w = W::new();
    let up = tx_frame(0x1234, 1, [1024; 8], &[]);
    let down = rx_frame(0x1234, 1, &[]);
    let mut rng = Lcg(3);
    // Dwells of ~5 s every 90 s; clocks 40 ppm apart; some frames lost.
    for dwell in 0..10u64 {
        let start = dwell * 90_000_000;
        for v in 0..4u64 {
            let k = (start + v * VISIT) / 32_000;
            let tick = k * 32_000 + k * 32_000 * 40 / 1_000_000;
            if (dwell + v) % 3 != 0 {
                w.observe(&up, &mlrs_rx(), meta((tick as i64 + 2_000 + rng.jitter(1_000)) as u64, -50 - (v as i16) * 15), &cfg);
            }
            w.observe(&down, &mlrs_rx(), meta((tick as i64 + 18_000 + rng.jitter(1_000)) as u64, -90), &cfg);
            let r = reports(&mut w, tick + 20_000, &cfg);
            assert!(r.is_empty(), "dwell {dwell} visit {v}: {r:?}");
        }
    }
    // Two links with different sync words, interleaved: also quiet.
    let other = tx_frame(0x7777, 1, [1024; 8], &[]);
    for v in 0..10u64 {
        let base = 2_000_000_000 + v * VISIT;
        w.observe(&up, &mlrs_rx(), meta(base + 2_000, -60), &cfg);
        w.observe(&other, &mlrs_rx(), meta(base + 600_000 + 9_000, -60), &cfg);
    }
    assert!(reports(&mut w, 2_100_000_000, &cfg).is_empty());
}

// ------------------------------------------------------------------ SiK --

fn sik_rx() -> RxConfig {
    crate::sik::rx_config(Region::Fcc915, 25, 0, 64).unwrap()
}

const BONUS: u16 = 1 << 14;

/// A packet sent at `t` claiming its window ends at `end`.
fn sik_pkt(netid: u16, serial: &[u8], t: u64, end: u64, flags: u16) -> std::vec::Vec<u8> {
    let w = ((end.saturating_sub(t)) / SIK_TICK_US) as u16 & 0x1FFF;
    crate::sik::encode(netid, serial, w | flags)
}

/// AES-capable build: the window trailer, then a 2-byte payload CRC.
fn sik_pkt_aes(netid: u16, serial: &[u8], t: u64, end: u64) -> std::vec::Vec<u8> {
    let w = ((end.saturating_sub(t)) / SIK_TICK_US) as u16 & 0x1FFF;
    let mut s = serial.to_vec();
    s.extend_from_slice(&w.to_le_bytes());
    crate::sik::encode(netid, &s, (t as u16).wrapping_mul(40_503))
}

/// One owner window on our channel: `n` packets 6 ms apart before `end`.
fn sik_window(w: &mut W, start: u64, n: u64, rng: &mut Lcg, cfg: &Config, aes: bool) -> u64 {
    let end = start + 40_000;
    for i in 0..n {
        let t = start + i * 6_000;
        let p = if aes { sik_pkt_aes(25, &[0x55; 20], t, end) } else { sik_pkt(25, &[0x55; 20], t, end, 0) };
        let stamp = (t as i64 + rng.jitter(800)) as u64;
        w.observe(&p, &sik_rx(), meta(stamp, -70 - (i as i16 % 2) * 25), cfg);
    }
    end
}

#[test]
fn sik_unsynced_third_radio_fires_after_three_windows() {
    let cfg = Config::default();
    let mut w = W::new();
    let mut rng = Lcg(11);
    // The pair alone for a while: windows agree.
    for v in 0..4u64 {
        sik_window(&mut w, 1_000_000 + v * 1_500_000, 5, &mut rng, &cfg, false);
    }
    assert!(reports(&mut w, 8_000_000, &cfg).is_empty());
    let mut out = std::vec::Vec::new();
    for v in 4..8u64 {
        let start = 1_000_000 + v * 1_500_000;
        let end = sik_window(&mut w, start, 3, &mut rng, &cfg, false);
        // The intruder transmits in the same window on its own clock.
        let t = start + 20_000;
        let p = sik_pkt(25, &[0x66; 10], t, end + 25_000, 0);
        w.observe(&p, &sik_rx(), meta(t, -50), &cfg);
        let t = start + 26_000;
        let p = sik_pkt(25, &[0x55; 20], t, end, 0);
        w.observe(&p, &sik_rx(), meta(t, -70), &cfg);
        out.extend(reports(&mut w, start + 100_000, &cfg));
    }
    let r = out.last().expect("contested");
    assert_eq!(r.system, System::Sik);
    assert_eq!(r.rules, RULE_CLOCKS);
    assert_eq!(r.transmitters, 3);
    // Not before the third contradicting window.
    assert!(out[0].events >= 1);
    assert_eq!(out.iter().map(|r| r.events).max(), Some(2));
}

#[test]
fn sik_pair_with_bonus_stats_jitter_and_loss_is_quiet() {
    let cfg = Config::default();
    let mut w = W::new();
    let mut rng = Lcg(5);
    for v in 0..30u64 {
        let start = 1_000_000 + v * 1_500_000;
        // Some visits lose packets.
        let n = if v % 4 == 0 { 1 } else { 4 };
        let end = sik_window(&mut w, start, n, &mut rng, &cfg, false);
        // A statistics packet (window 0) and the other radio's bonus
        // packets on yielded ticks: same window end.
        w.observe(&crate::sik::encode(25, &[1; 12], 0), &sik_rx(), meta(start + 30_000, -70), &cfg);
        let t = start + 33_000;
        w.observe(&sik_pkt(25, &[2; 8], t, end, BONUS), &sik_rx(), meta(t + 500, -95), &cfg);
        assert!(reports(&mut w, start + 100_000, &cfg).is_empty(), "visit {v}");
    }
}

#[test]
fn sik_aes_trailer_layout_is_learned() {
    let cfg = Config::default();
    let mut w = W::new();
    let mut rng = Lcg(9);
    // Read at the 2-byte offset these trailers are noise: never trusted.
    for v in 0..12u64 {
        sik_window(&mut w, 1_000_000 + v * 1_500_000, 5, &mut rng, &cfg, true);
    }
    assert!(reports(&mut w, 30_000_000, &cfg).is_empty());
    let mut out = std::vec::Vec::new();
    for v in 12..16u64 {
        let start = 1_000_000 + v * 1_500_000;
        let end = sik_window(&mut w, start, 3, &mut rng, &cfg, true);
        let t = start + 20_000;
        w.observe(&sik_pkt_aes(25, &[0x66; 10], t, end + 25_000), &sik_rx(), meta(t, -50), &cfg);
        let t = start + 26_000;
        w.observe(&sik_pkt_aes(25, &[0x55; 20], t, end), &sik_rx(), meta(t, -70), &cfg);
        out.extend(reports(&mut w, start + 100_000, &cfg));
    }
    assert_eq!(out.last().map(|r| r.rules), Some(RULE_CLOCKS));
}

#[test]
fn sik_two_netids_are_two_links() {
    let cfg = Config::default();
    let mut w = W::new();
    for v in 0..20u64 {
        let start = 1_000_000 + v * 1_500_000;
        let end = start + 40_000;
        for i in 0..3 {
            let t = start + i * 6_000;
            w.observe(&sik_pkt(25, &[0x55; 20], t, end, 0), &sik_rx(), meta(t, -70), &cfg);
            // Another pair with its own NET ID and clock, interleaved.
            let t2 = t + 3_000;
            w.observe(&sik_pkt(77, &[0x55; 20], t2, end + 17_000, 0), &sik_rx(), meta(t2, -70), &cfg);
        }
    }
    assert_eq!(w.len(), 2);
    assert!(reports(&mut w, 60_000_000, &cfg).is_empty());
}

fn heartbeat(seq: u8, sys: u8, comp: u8) -> std::vec::Vec<u8> {
    crate::mavlink::encode_v1(seq, sys, comp, crate::mavlink::MSG_HEARTBEAT, &crate::mavlink::heartbeat_payload())
}

/// One window in which a time-synced injector (SiKW00F) and the owner both
/// send GCS heartbeats (system 255, component 190).
fn sik_synced_injection(w: &mut W, cfg: &Config, start: u64, seq: u8) {
    let end = start + 40_000;
    let mut pkts = std::vec::Vec::new();
    pkts.push((start, heartbeat(seq, 255, 190)));
    pkts.push((start + 6_000, heartbeat(seq.wrapping_add(1), 255, 190)));
    pkts.push((start + 12_000, heartbeat(seq.wrapping_add(120), 255, 190))); // injector
    pkts.push((start + 18_000, heartbeat(seq.wrapping_add(2), 255, 190)));
    for (t, s) in pkts {
        w.observe(&sik_pkt(25, &s, t, end, 0), &sik_rx(), meta(t, -70), cfg);
    }
}

#[test]
fn sik_synced_injector_needs_content_and_two_sequences() {
    let off = Config::default();
    let mut w = W::new();
    for v in 0..6u64 {
        sik_synced_injection(&mut w, &off, 1_000_000 + v * 1_500_000, (v * 40) as u8);
    }
    assert!(reports(&mut w, 20_000_000, &off).is_empty(), "same clock, no content: cannot tell");

    let on = Config {
        decode_content: true,
        read_ids: false,
    };
    let mut w = W::new();
    let mut out = std::vec::Vec::new();
    for v in 0..3u64 {
        sik_synced_injection(&mut w, &on, 1_000_000 + v * 1_500_000, (v * 40) as u8);
        out.extend(reports(&mut w, 1_100_000 + v * 1_500_000, &on));
    }
    let r = out.last().expect("contested");
    assert_eq!(r.rules, RULE_SEQUENCE);
    assert_eq!(r.transmitters, 3);
    assert_eq!(r.id, None);
}

#[test]
fn sik_one_sender_with_resends_and_loss_is_quiet() {
    let on = Config {
        decode_content: true,
        read_ids: true,
    };
    let mut w = W::new();
    let mut seq = 0u8;
    for v in 0..20u64 {
        let start = 1_000_000 + v * 1_500_000;
        let end = start + 40_000;
        for i in 0..5u64 {
            let t = start + i * 6_000;
            // A resend repeats a frame; a lost packet skips a few.
            if i != 2 {
                seq = seq.wrapping_add(if i == 4 { 3 } else { 1 });
            }
            w.observe(&sik_pkt(25, &heartbeat(seq, 1, 1), t, end, 0), &sik_rx(), meta(t, -70), &on);
            // The other direction (GCS) in bonus ticks: its own count.
            w.observe(&sik_pkt(25, &heartbeat(seq.wrapping_mul(3), 255, 190), t + 2_000, end, BONUS), &sik_rx(), meta(t + 2_000, -90), &on);
        }
        // Between visits the stream has moved on by any amount.
        seq = seq.wrapping_add(97);
    }
    assert!(reports(&mut w, 60_000_000, &on).is_empty());
}

// ---------------------------------------------------------------- misc --

#[test]
fn detail_round_trips_and_ignores_other_probes() {
    for (s, n, r) in [(System::Sik, 3, RULE_CLOCKS | RULE_SEQUENCE), (System::Mlrs, 15, RULE_PHASE)] {
        let d = decode_detail(encode_detail(s, n, r, Region::Eu868));
        assert_eq!((d.system, d.transmitters, d.rules, d.region), (Some(s), n, r, Some(Region::Eu868)));
    }
    assert_eq!(decode_detail(encode_detail(System::ExpressLrs, 200, 0, Region::Ism2g4)).transmitters, 15);
    assert_eq!(rule_name(RULE_HANDOVER), "new transmitter after silence");
    // OpenLRSng and energy probes are not used.
    let mut w = W::new();
    let rx = crate::openlrs::rx_config(Region::Eu433, 435_000_000, 2, false).unwrap();
    w.observe(&[0u8; 32], &rx, meta(1, -50), &Config::default());
    assert!(w.is_empty());
}

#[test]
fn table_is_bounded() {
    let cfg = Config::default();
    let mut w: Watch<2> = Watch::new();
    for k in 0..5u16 {
        w.observe(&sik_pkt(100 + k, &[0; 4], 1_000, 30_000, 0), &sik_rx(), meta(1_000 + u64::from(k), -60), &cfg);
    }
    assert_eq!(w.len(), 2);
    assert_eq!(w.evicted, 3);
}
