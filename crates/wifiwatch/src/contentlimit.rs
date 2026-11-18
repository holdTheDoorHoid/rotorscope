//! Report limiter keyed on (address, content), for detectors that forward a
//! transmitter's element to the hub at most about once a second (node-wifi's
//! DJI Wi-Fi DroneID and Wi-Fi drone reports).
//!
//! Keyed on the address alone, the first frame after each one-second gap
//! wins: someone sending ~100 frames a second from a drone's own address,
//! carrying a false position, gets forwarded almost every second and the
//! drone's real element almost never. Here each distinct content from an
//! address has its own one-second gap, so the forger's content and the
//! drone's are both forwarded. Bounded: an address may have at most
//! `per_source` contents forwarded within one gap (a forger sending many
//! different contents gets that many a second, not one per frame). With
//! `per_source` 2, a drone whose element changes with every beacon is
//! forwarded twice a second (once before), and still once a second while a
//! forger holds the other place.
//!
//! **Alternating contents.** A real transmitter's reports move forward: a
//! DJI DroneID element changes with every beacon (its sequence number), a
//! drone's network name does not change at all. One address whose frames go
//! back to a content it sent before, after a different one (A, B, A), is two
//! transmitters using one address: a spoof. [`Verdict::alternating`] says so
//! for every frame of that address for [`FLAG_MS`] after the last
//! alternation, and [`ContentLimit::alternations`] counts them.
//!
//! Limits: a forger sending many *different* contents from the drone's
//! address takes the address's places as fast as they free, so the drone's
//! own report gets one only sometimes; the address is flagged either way.

use crate::util::{Ms, elapsed};

/// How long an address stays flagged after its last alternation.
pub const FLAG_MS: Ms = 60_000;
/// Addresses remembered as alternating (the stalest flag is replaced).
pub const FLAGGED: usize = 8;

#[derive(Clone, Copy, Debug)]
struct Entry {
    mac: [u8; 6],
    content: u32,
    /// Last forwarded, and last heard (for eviction and "latest content").
    sent: Ms,
    heard: Ms,
}

/// The decision for one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// Forward this report.
    pub send: bool,
    /// This address has alternated between contents within [`FLAG_MS`].
    pub alternating: bool,
}

/// `N` (address, content) entries; the stalest is replaced when full.
#[derive(Clone, Debug)]
pub struct ContentLimit<const N: usize> {
    seen: heapless::Vec<Entry, N>,
    flagged: heapless::Vec<([u8; 6], Ms), FLAGGED>,
    gap_ms: Ms,
    per_source: u8,
    /// Alternations seen (A, B, A from one address), total.
    pub alternations: u32,
}

impl<const N: usize> ContentLimit<N> {
    /// At most one report per (address, content) every `gap_ms`, and at
    /// most `per_source` contents per address within one gap.
    pub const fn new(gap_ms: Ms, per_source: u8) -> Self {
        Self { seen: heapless::Vec::new(), flagged: heapless::Vec::new(), gap_ms, per_source, alternations: 0 }
    }

    /// One report from `mac` whose content hashes to `content`, at `now`.
    pub fn check(&mut self, mac: [u8; 6], content: u32, now: Ms) -> Verdict {
        // The content this address sent most recently (before this frame).
        let latest = self
            .seen
            .iter()
            .filter(|e| e.mac == mac)
            .min_by_key(|e| elapsed(now, e.heard))
            .map(|e| e.content);
        let send = match self.seen.iter_mut().find(|e| e.mac == mac && e.content == content) {
            Some(e) => {
                if latest.is_some_and(|l| l != content) {
                    self.alternations = self.alternations.wrapping_add(1);
                    Self::flag(&mut self.flagged, mac, now);
                }
                e.heard = now;
                if elapsed(now, e.sent) >= self.gap_ms {
                    e.sent = now;
                    true
                } else {
                    false
                }
            }
            None => {
                let gap = self.gap_ms;
                let active = self.seen.iter().filter(|e| e.mac == mac && elapsed(now, e.sent) < gap).count();
                if active >= usize::from(self.per_source) {
                    false
                } else {
                    if self.seen.is_full()
                        && let Some(i) = (0..self.seen.len()).max_by_key(|&i| elapsed(now, self.seen[i].heard))
                    {
                        self.seen.swap_remove(i);
                    }
                    let _ = self.seen.push(Entry { mac, content, sent: now, heard: now });
                    true
                }
            }
        };
        Verdict { send, alternating: self.is_flagged(mac, now) }
    }

    fn flag(flagged: &mut heapless::Vec<([u8; 6], Ms), FLAGGED>, mac: [u8; 6], now: Ms) {
        if let Some(f) = flagged.iter_mut().find(|f| f.0 == mac) {
            f.1 = now;
            return;
        }
        if flagged.is_full()
            && let Some(i) = (0..flagged.len()).max_by_key(|&i| elapsed(now, flagged[i].1))
        {
            flagged.swap_remove(i);
        }
        let _ = flagged.push((mac, now));
    }

    /// `mac` alternated between contents within [`FLAG_MS`].
    pub fn is_flagged(&self, mac: [u8; 6], now: Ms) -> bool {
        self.flagged.iter().any(|f| f.0 == mac && elapsed(now, f.1) <= FLAG_MS)
    }

    /// Addresses flagged now.
    pub fn flagged(&self, now: Ms) -> usize {
        self.flagged.iter().filter(|f| elapsed(now, f.1) <= FLAG_MS).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::RateLimit;

    const VICTIM: [u8; 6] = [0x60, 0x60, 0x1F, 0xAA, 0xBB, 0xCC];
    const GAP: Ms = 1000;

    /// The old node-wifi limiter (detect.rs before this change): one entry
    /// per address, first frame after the gap wins. Same semantics as
    /// `util::RateLimit` with one kind.
    fn old(l: &mut RateLimit<16>, mac: [u8; 6], now: Ms) -> bool {
        l.allow(0, mac, now)
    }

    /// 10 s: the forger sends 100 frames a second from the victim's address
    /// with one false content; the victim beacons 10 times a second, its
    /// content `victim(i)` for beacon `i`. Returns how many victim reports
    /// were forwarded, new limiter and old.
    fn spoof_run(per_source: u8, victim: impl Fn(u32) -> u32) -> (u32, u32, ContentLimit<24>) {
        let mut new = ContentLimit::<24>::new(GAP, per_source);
        let mut legacy = RateLimit::<16>::new(GAP);
        let (mut got_new, mut got_old) = (0, 0);
        for ms in 0..10_000u32 {
            if ms % 10 == 3 {
                new.check(VICTIM, 0xBAD, ms);
                old(&mut legacy, VICTIM, ms);
            }
            if ms % 100 == 50 {
                let c = victim(ms / 100);
                if new.check(VICTIM, c, ms).send {
                    got_new += 1;
                }
                if old(&mut legacy, VICTIM, ms) {
                    got_old += 1;
                }
            }
        }
        (got_new, got_old, new)
    }

    #[test]
    fn spoofed_address_does_not_suppress_a_changing_report() {
        // DJI DroneID: every beacon's element differs (sequence number).
        let (got_new, got_old, l) = spoof_run(4, |i| 1000 + i);
        // per_source - 1 places a second left after the forger's one.
        assert!(got_new >= 27, "victim forwarded {got_new} times");
        let (got_new, _, _) = spoof_run(2, |i| 1000 + i);
        assert!(got_new >= 9, "victim forwarded {got_new} times");
        assert_eq!(got_old, 0, "the old limiter let the forger win every second");
        assert!(l.is_flagged(VICTIM, 10_000));
        assert!(l.alternations > 0);
    }

    #[test]
    fn spoofed_address_does_not_suppress_a_constant_report() {
        // A Wi-Fi drone: its network name never changes.
        let (got_new, got_old, l) = spoof_run(2, |_| 7);
        assert!(got_new >= 9, "victim forwarded {got_new} times");
        assert_eq!(got_old, 0);
        assert!(l.is_flagged(VICTIM, 10_000));
    }

    #[test]
    fn one_content_is_still_once_per_gap() {
        let mut l = ContentLimit::<24>::new(GAP, 4);
        let sent = (0..10_000u32).filter(|&ms| l.check(VICTIM, 5, ms).send).count();
        assert_eq!(sent, 10);
        assert_eq!(l.alternations, 0);
    }

    #[test]
    fn many_contents_from_one_address_are_bounded() {
        let mut l = ContentLimit::<24>::new(GAP, 4);
        // A new content every millisecond for 10 s.
        let sent = (0..10_000u32).filter(|&ms| l.check(VICTIM, ms, ms).send).count();
        assert!(sent <= 4 * 10 + 4, "{sent}");
        assert!(sent >= 4 * 9, "{sent}");
    }

    #[test]
    fn honest_transmitters_are_never_flagged() {
        let mut l = ContentLimit::<24>::new(GAP, 4);
        let other = [0x02, 1, 2, 3, 4, 5];
        for ms in (0..60_000u32).step_by(100) {
            // Changing content (DJI), constant content (Wi-Fi drone).
            let v = l.check(VICTIM, ms, ms);
            let w = l.check(other, 42, ms);
            assert!(!v.alternating && !w.alternating);
        }
        assert_eq!(l.alternations, 0);
        assert_eq!(l.flagged(60_000), 0);
    }

    #[test]
    fn alternation_is_flagged_and_expires() {
        let mut l = ContentLimit::<24>::new(GAP, 4);
        assert!(!l.check(VICTIM, 1, 0).alternating);
        assert!(!l.check(VICTIM, 2, 10).alternating);
        // Back to 1: two transmitters.
        assert!(l.check(VICTIM, 1, 20).alternating);
        assert_eq!(l.alternations, 1);
        // Other addresses are not affected.
        assert!(!l.check([0x02, 0, 0, 0, 0, 1], 1, 30).alternating);
        assert!(l.is_flagged(VICTIM, 20 + FLAG_MS));
        assert!(!l.is_flagged(VICTIM, 21 + FLAG_MS));
        assert!(!l.check(VICTIM, 3, 21 + FLAG_MS).alternating);
    }

    #[test]
    fn a_random_address_flood_does_not_stop_reports() {
        // Many addresses, one frame each: each is forwarded (the hub queue's
        // budget, `shed`, bounds the volume), and the victim still is.
        let mut l = ContentLimit::<24>::new(GAP, 4);
        let mut got = 0;
        for ms in 0..10_000u32 {
            let b = ms.to_le_bytes();
            l.check([0x02, b[0], b[1], b[2], b[3], 9], 1, ms);
            if ms % 1000 == 500 && l.check(VICTIM, 77, ms).send {
                got += 1;
            }
        }
        assert_eq!(got, 10);
    }
}
