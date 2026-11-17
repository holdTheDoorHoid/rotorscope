//! Per-link packet counts and report rate limiting, like `openlink`'s
//! table. The receive path calls [`Sightings::record`] for every
//! [`ControlSighting`] (a few compares, no allocation); a task calls
//! [`Sightings::take_due`] a few times a second.
//!
//! False-positive discipline: a link seen only as [`Confidence::Likely`]
//! (right radio settings and size, nothing checkable) is not reported until
//! it has `min_likely` packets; one confirmed packet makes it reportable at
//! once.

use crate::{Confidence, ControlSighting, LinkId, Region, System};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sighting {
    /// The latest packet's sighting.
    pub last: ControlSighting,
    /// Best confidence seen for this link.
    pub best: Confidence,
    pub packets: u32,
    pub confirmed: u32,
    pub first_us: u64,
    pub last_us: u64,
    /// Strongest RSSI seen, dBm.
    pub peak_rssi_dbm: i16,
    reported_us: Option<u64>,
    reported_packets: u32,
}

#[derive(PartialEq, Eq)]
struct Key {
    system: System,
    id: Option<LinkId>,
    region: Region,
}

impl Key {
    fn of(s: &ControlSighting) -> Self {
        Self {
            system: s.system,
            id: s.id,
            region: s.region,
        }
    }
}

/// Fixed table of `N` links; when full, the least recently heard is
/// replaced (counted in `evicted`).
#[derive(Debug, Clone)]
pub struct Sightings<const N: usize> {
    slots: [Option<Sighting>; N],
    pub evicted: u32,
    /// Packets a Likely-only link needs before it is reported.
    pub min_likely: u32,
}

impl<const N: usize> Default for Sightings<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Sightings<N> {
    pub const fn new() -> Self {
        Self {
            slots: [None; N],
            evicted: 0,
            min_likely: 3,
        }
    }

    pub fn record(&mut self, s: &ControlSighting) {
        let key = Key::of(s);
        let mut free = None;
        let mut oldest: Option<(usize, u64)> = None;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            match slot {
                Some(e) if Key::of(&e.last) == key => {
                    e.last = *s;
                    e.best = e.best.max(s.confidence);
                    e.packets = e.packets.saturating_add(1);
                    if s.confidence == Confidence::Confirmed {
                        e.confirmed = e.confirmed.saturating_add(1);
                    }
                    e.last_us = s.t_us;
                    e.peak_rssi_dbm = e.peak_rssi_dbm.max(s.rssi_dbm);
                    return;
                }
                Some(e) => {
                    if oldest.is_none_or(|(_, t)| e.last_us < t) {
                        oldest = Some((i, e.last_us));
                    }
                }
                None => {
                    free.get_or_insert(i);
                }
            }
        }
        let at = match (free, oldest) {
            (Some(i), _) => i,
            (None, Some((i, _))) => {
                self.evicted = self.evicted.saturating_add(1);
                i
            }
            (None, None) => return,
        };
        self.slots[at] = Some(Sighting {
            last: *s,
            best: s.confidence,
            packets: 1,
            confirmed: (s.confidence == Confidence::Confirmed) as u32,
            first_us: s.t_us,
            last_us: s.t_us,
            peak_rssi_dbm: s.rssi_dbm,
            reported_us: None,
            reported_packets: 0,
        });
    }

    /// Hand every link due a report to `out` and mark it reported. Due:
    /// reportable (confirmed, energy, or `min_likely` packets), new packets
    /// since the last report, and the last report at least `min_gap_us` ago.
    pub fn take_due(&mut self, now_us: u64, min_gap_us: u64, mut out: impl FnMut(&Sighting)) {
        let min_likely = self.min_likely;
        for s in self.slots.iter_mut().flatten() {
            let reportable = s.best != Confidence::Likely || s.packets >= min_likely;
            let fresh = s.packets != s.reported_packets;
            let gap_ok = s
                .reported_us
                .is_none_or(|r| now_us.saturating_sub(r) >= min_gap_us);
            if reportable && fresh && gap_ok {
                out(s);
                s.reported_us = Some(now_us);
                s.reported_packets = s.packets;
            }
        }
    }

    /// Forget links not heard for `max_age_us`.
    pub fn expire(&mut self, now_us: u64, max_age_us: u64) {
        for slot in self.slots.iter_mut() {
            if slot.is_some_and(|s| now_us.saturating_sub(s.last_us) > max_age_us) {
                *slot = None;
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Sighting> {
        self.slots.iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Direction, Probe};

    fn s(conf: Confidence, t: u64) -> ControlSighting {
        ControlSighting {
            system: System::ExpressLrs,
            region: Region::Fcc915,
            probe: Probe::Energy,
            freq_hz: 915_500_000,
            rssi_dbm: -70,
            snr_db: 5,
            t_us: t,
            confidence: conf,
            direction: Direction::Unknown,
            id: None,
            version: Some(4),
        }
    }

    #[test]
    fn likely_needs_three_packets_confirmed_reports_at_once() {
        let mut t = Sightings::<4>::new();
        t.record(&s(Confidence::Likely, 1));
        t.record(&s(Confidence::Likely, 2));
        let mut n = 0;
        t.take_due(10, 1_000_000, |_| n += 1);
        assert_eq!(n, 0);
        t.record(&s(Confidence::Likely, 3));
        t.take_due(11, 1_000_000, |x| {
            n += 1;
            assert_eq!(x.packets, 3);
        });
        assert_eq!(n, 1);
        // Rate limited.
        t.record(&s(Confidence::Confirmed, 12));
        t.take_due(13, 1_000_000, |_| n += 1);
        assert_eq!(n, 1);
        t.take_due(1_000_020, 1_000_000, |x| {
            n += 1;
            assert_eq!(x.best, Confidence::Confirmed);
        });
        assert_eq!(n, 2);
        t.expire(10_000_000, 1_000_000);
        assert_eq!(t.iter().count(), 0);
    }
}
