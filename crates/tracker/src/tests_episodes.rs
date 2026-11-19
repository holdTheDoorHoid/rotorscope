//! Threat episodes (WP-14a, TR-16): every time a threat is raised it is
//! announced (console line and evidence record), every time it ends that is
//! announced with what the episode totalled, a re-raise counts as unseen by
//! the operator, and a flapping threat is held to a bounded number of
//! records, saying so.

extern crate std;

use link::{ThreatKind, ThreatSighting};

use crate::threats::{EndWhy, FLAP_QUIET_MS, FLAP_RAISES, FLAP_WINDOW_MS, THREAT_ACTIVE_MS, ThreatId};
use crate::*;

fn deauth(count: u32, level: i8) -> ThreatSighting {
    ThreatSighting {
        kind: ThreatKind::Deauth,
        channel: 6,
        freq: 24_370,
        width_khz: 0,
        level,
        count,
        window_ms: 1000,
        source: [0x02, 0, 0, 0, 0, 1],
        target: [0x0a, 0, 0, 0, 0, 2],
        detail: 0,
        ssid: heapless::String::new(),
        uptime_us: 1,
    }
}

fn raised(tr: &mut Tracker, now: u64) -> std::vec::Vec<ThreatId> {
    let mut v = std::vec::Vec::new();
    while let Some(id) = tr.take_new_threat(now) {
        v.push(id);
    }
    v
}

fn ended(tr: &mut Tracker, now: u64) -> std::vec::Vec<threats::EpisodeEnd> {
    let mut v = std::vec::Vec::new();
    while let Some(e) = tr.take_ended_threat(now) {
        v.push(e);
    }
    v
}

/// The attack in TR-16: trip the alarm briefly, let it end, attack again
/// inside the ten minutes the entry is kept. The second attack must be
/// announced again (it used not to be: no console line, no evidence).
#[test]
fn a_threat_raised_again_after_it_ended_is_announced_again() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_threat(1, &deauth(50, -40), 1_000).unwrap();
    assert_eq!(raised(&mut tr, 1_000), [a]);
    // Quiet for a minute: it ended (and stays listed as ended).
    let later = 1_000 + 60_000;
    assert!(!tr.get_threat(a).unwrap().active(later));
    // Raised again: same table entry, announced again.
    let b = tr.ingest_threat(1, &deauth(70, -30), later).unwrap();
    assert_eq!(a, b, "same table entry");
    assert_eq!(raised(&mut tr, later), [a], "the second attack is announced");
    assert_eq!(tr.get_threat(a).unwrap().episode(), 2);
}

#[test]
fn an_episode_end_is_announced_once_with_its_totals() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_threat(1, &deauth(50, -40), 1_000).unwrap();
    tr.ingest_threat(1, &deauth(30, -35), 2_000);
    tr.ingest_threat(1, &deauth(20, -50), 5_000);
    assert_eq!(raised(&mut tr, 5_000), [a]);
    // Still active: nothing ended.
    assert!(ended(&mut tr, 5_000 + THREAT_ACTIVE_MS - 1).is_empty());
    let e = ended(&mut tr, 5_000 + THREAT_ACTIVE_MS);
    assert_eq!(e.len(), 1);
    let e = &e[0];
    assert_eq!(e.id, a);
    assert_eq!(e.kind, ThreatKind::Deauth);
    assert_eq!(e.episode, 1);
    assert_eq!((e.started_ms, e.last_ms), (1_000, 5_000));
    assert_eq!(e.duration_ms(), 4_000);
    assert_eq!(e.count, 100);
    assert_eq!(e.reports, 3);
    assert_eq!(e.peak_level, -35);
    assert_eq!(e.raises, 1);
    assert_eq!(e.why, EndWhy::Quiet);
    // Once.
    assert!(ended(&mut tr, 60_000).is_empty());

    // The second episode's totals are its own, not the entry's.
    tr.ingest_threat(1, &deauth(5, -60), 100_000);
    assert_eq!(raised(&mut tr, 100_000), [a]);
    let e = ended(&mut tr, 100_000 + THREAT_ACTIVE_MS);
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].episode, e[0].count, e[0].reports, e[0].peak_level), (2, 5, 1, -60));
    // The entry keeps its whole-life totals for the screen.
    assert_eq!(tr.get_threat(a).unwrap().count, 105);
}

#[test]
fn a_re_raise_counts_as_unseen_by_the_operator() {
    let mut tr: Tracker = Tracker::default();
    tr.ingest_threat(1, &deauth(50, -40), 1_000).unwrap();
    raised(&mut tr, 1_000);
    // The operator looked at 2 s: one nuisance alarm seen.
    assert_eq!(tr.alarm_counts(2_000, 2_000), (1, 0));
    // It ends, and is raised again at 60 s. A "Mute" pressed with the 2 s
    // view must not silence the new episode the operator has not seen.
    tr.ingest_threat(1, &deauth(50, -40), 60_000);
    raised(&mut tr, 60_000);
    assert_eq!(tr.alarm_counts(2_000, 60_000), (0, 0));
    assert!(tr.mute_seen(2_000, 60_000).is_empty());
    // Seen at 61 s: now it can be muted.
    assert_eq!(tr.alarm_counts(61_000, 61_000), (1, 0));
    // The entry still says when it was first seen at all.
    let t = &tr.threats()[0];
    assert_eq!((t.first_seen_ms, t.raised_ms), (1_000, 60_000));
}

/// A threat that flaps (reported, quiet just long enough to end, reported
/// again) must not fill the log: after [`FLAP_RAISES`] raises inside
/// [`FLAP_WINDOW_MS`] its end is held until it has been quiet for
/// [`FLAP_QUIET_MS`], further raises are counted into the episode, and the
/// end says how many there were.
#[test]
fn a_flapping_threat_is_bounded_and_says_so() {
    let mut tr: Tracker = Tracker::default();
    let period = THREAT_ACTIVE_MS + 1_000; // ends, then comes back
    let mut raises = 0;
    let mut ends = 0;
    let mut last_raise_note = 0;
    let mut t = 1_000u64;
    // Ten minutes of flapping.
    while t < 600_000 {
        tr.ingest_threat(1, &deauth(10, -40), t);
        for id in raised(&mut tr, t) {
            raises += 1;
            last_raise_note = tr.get_threat(id).unwrap().raises_in_window();
        }
        // Run the loop every second in between, as the hub does.
        for s in 1..=(period / 1000) {
            ends += ended(&mut tr, t + s * 1000).len();
        }
        t += period;
    }
    // Without the bound this would be ~55 raises and ~55 ends.
    assert_eq!(raises, FLAP_RAISES as usize, "raised {raises} times");
    assert_eq!(ends, FLAP_RAISES as usize - 1, "ended {ends} times");
    assert_eq!(last_raise_note, FLAP_RAISES, "the raise that starts the hold says how many raises the window held");
    // Quiet at last: one end, holding every raise since.
    let e = ended(&mut tr, t + FLAP_QUIET_MS);
    assert_eq!(e.len(), 1);
    assert!(e[0].held, "the end says the episode was held as flapping");
    assert!(e[0].raises > 40, "{}", e[0].raises);
    assert_eq!(e[0].why, EndWhy::Quiet);
}

/// The hold is no hiding place: a flapping threat is shown active on the
/// screen whenever it is reported, and an attack after the hold ends is a
/// new, announced episode.
#[test]
fn after_a_flapping_hold_a_new_attack_is_announced() {
    let mut tr: Tracker = Tracker::default();
    let mut t = 1_000u64;
    for _ in 0..FLAP_RAISES + 2 {
        tr.ingest_threat(1, &deauth(10, -40), t);
        raised(&mut tr, t);
        ended(&mut tr, t + THREAT_ACTIVE_MS);
        t += THREAT_ACTIVE_MS + 1_000;
    }
    // Reported inside the hold: active on the screen, unseen by the operator.
    tr.ingest_threat(1, &deauth(10, -40), t);
    assert!(raised(&mut tr, t).is_empty(), "held: counted, not logged again");
    assert_eq!(tr.active_threats(t).count(), 1);
    assert_eq!(tr.alarm_counts(t - 1, t), (0, 0), "a raise inside the hold is still unseen");
    // Quiet long enough: the held episode ends; the next attack is new.
    let quiet = t + FLAP_QUIET_MS;
    assert_eq!(ended(&mut tr, quiet).len(), 1);
    let at = quiet + FLAP_WINDOW_MS;
    tr.ingest_threat(1, &deauth(10, -40), at);
    assert_eq!(raised(&mut tr, at).len(), 1);
}

/// A threat removed from the table (it made room for a newer one, or its
/// entry expired) while its episode was open still gets its end record.
#[test]
fn an_episode_removed_from_the_table_still_ends() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_threat(1, &deauth(50, -40), 1_000).unwrap();
    raised(&mut tr, 1_000);
    // Expired before the loop asked (removal after 10 minutes).
    let gone = 1_000 + tr.config().remove_after_ms + 1;
    tr.expire(gone);
    assert!(tr.get_threat(a).is_none());
    let e = ended(&mut tr, gone);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].id, a);
}

#[test]
fn an_active_flood_pushed_out_of_a_full_table_ends_as_removed() {
    let mut tr: Tracker = Tracker::default();
    let mut s = deauth(1, -40);
    let first = tr.ingest_threat(1, &s, 1_000).unwrap();
    assert_eq!(raised(&mut tr, 1_000), [first]);
    // Fill the flood quota from other nodes, all active.
    for n in 2..=(threats::MAX_FLOOD_THREATS as u8 + 1) {
        s.source[5] = n;
        tr.ingest_threat(n, &s, 1_000 + u64::from(n));
    }
    raised(&mut tr, 2_000);
    assert!(tr.get_threat(first).is_none(), "the oldest flood made room");
    let e = ended(&mut tr, 2_000);
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].id, e[0].why), (first, EndWhy::Removed));
}

/// A muted threat is not announced, so its end is not either (there is no
/// raise record for it to close).
#[test]
fn a_muted_episode_has_no_end_record() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_threat(1, &deauth(50, -40), 1_000).unwrap();
    raised(&mut tr, 1_000);
    tr.silence(a, 1_500).unwrap();
    // The end of the episode that was announced (before the mute) is due.
    assert_eq!(ended(&mut tr, 1_000 + THREAT_ACTIVE_MS).len(), 1);
    // A new episode while muted: neither raised nor ended.
    tr.ingest_threat(1, &deauth(50, -40), 60_000);
    assert!(raised(&mut tr, 60_000).is_empty());
    assert!(ended(&mut tr, 60_000 + THREAT_ACTIVE_MS).is_empty());
}

#[test]
fn episode_texts_say_what_happened() {
    let mut tr: Tracker = Tracker::default();
    let a = tr.ingest_threat(1, &deauth(50, -40), 1_000).unwrap();
    raised(&mut tr, 1_000);
    let mut s = std::string::String::new();
    threats::raise_note(tr.get_threat(a).unwrap(), &mut s).unwrap();
    assert_eq!(s, "", "a first raise reads as before");
    let e = ended(&mut tr, 11_000);
    let mut s = std::string::String::new();
    threats::end_text(&e[0], &mut s).unwrap();
    assert_eq!(s, "ended: Wi-Fi deauth attack (node 1), episode 1: active 0.0 s, 50 frames in 1 reports, peak -40 dBm");
    // Two quick re-raises: the third says the hold begins.
    for t in [12_000u64, 23_000] {
        tr.ingest_threat(1, &deauth(5, -40), t);
        raised(&mut tr, t);
        ended(&mut tr, t + THREAT_ACTIVE_MS);
    }
    let mut s = std::string::String::new();
    threats::raise_note(tr.get_threat(a).unwrap(), &mut s).unwrap();
    assert_eq!(
        s,
        " - raised again (episode 3; 3 raises within a minute); flapping: until it is quiet for 60 s, further raises are counted into this episode and reported when it ends"
    );
    // The hold, then its end.
    tr.ingest_threat(1, &deauth(5, -40), 34_000);
    let e = ended(&mut tr, 34_000 + FLAP_QUIET_MS);
    let mut s = std::string::String::new();
    threats::end_text(&e[0], &mut s).unwrap();
    assert!(s.contains("episode 3: active 11.0 s, 10 frames in 2 reports"), "{s}");
    assert!(s.contains("raised 2 times in this episode (flapping"), "{s}");
}
