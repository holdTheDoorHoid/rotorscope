//! A node's per-link packet counts and report rate limit.
//!
//! An FPV video downlink is hundreds to thousands of frames a second, all
//! saying the same thing. The node's receive callback calls
//! [`Sightings::record`] for each one (a few compares and a counter bump, no
//! allocation, no queue), and a task calls [`Sightings::take_due`] a few times
//! a second to send at most one report per link per interval, carrying the
//! running packet count.

use crate::{Direction, OpenLink, System};

/// One link in the table: its latest frame and a running count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sighting {
    /// The most recent frame's classification. Its `tx` follows the latest
    /// frame (OpenHD's changes with every packet).
    pub link: OpenLink,
    pub channel: u8,
    /// RSSI of the most recent frame, dBm.
    pub rssi: i8,
    /// Frames counted since this link entered the table. Saturates.
    pub packets: u32,
    /// Caller time (e.g. node uptime, µs) of the first and latest frame.
    pub first_us: u64,
    pub last_us: u64,
    reported_us: Option<u64>,
    reported_packets: u32,
}

/// What makes two frames "the same link" for counting and rate limiting:
/// one stream of one link, in one direction. (For OpenHD, which has no link
/// ID in the clear, that is every OpenHD unit on the channel using that port.)
#[derive(PartialEq, Eq)]
struct Key {
    system: System,
    id: Option<u32>,
    port: u8,
    direction: Direction,
}

impl Key {
    fn of(l: &OpenLink) -> Self {
        Self {
            system: l.system,
            id: l.id,
            port: l.port,
            direction: l.direction,
        }
    }
}

/// Fixed table of `N` links. When it is full a new link replaces the one
/// heard least recently (counted in [`Sightings::evicted`]).
#[derive(Debug, Clone)]
pub struct Sightings<const N: usize> {
    slots: [Option<Sighting>; N],
    /// Entries replaced to make room for a new link.
    pub evicted: u32,
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
        }
    }

    /// Count one frame. Cheap enough for a receive callback: one pass over
    /// at most `N` entries.
    pub fn record(&mut self, link: &OpenLink, channel: u8, rssi: i8, now_us: u64) {
        let key = Key::of(link);
        let mut free = None;
        let mut oldest: Option<(usize, u64)> = None;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            match slot {
                Some(s) if Key::of(&s.link) == key => {
                    s.link = *link;
                    s.channel = channel;
                    s.rssi = rssi;
                    s.packets = s.packets.saturating_add(1);
                    s.last_us = now_us;
                    return;
                }
                Some(s) => {
                    if oldest.is_none_or(|(_, t)| s.last_us < t) {
                        oldest = Some((i, s.last_us));
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
            // N == 0: nowhere to keep anything.
            (None, None) => return,
        };
        self.slots[at] = Some(Sighting {
            link: *link,
            channel,
            rssi,
            packets: 1,
            first_us: now_us,
            last_us: now_us,
            reported_us: None,
            reported_packets: 0,
        });
    }

    /// Hand every link that is due a report to `out`, and mark it reported.
    /// Due means: frames arrived since its last report, and the last report
    /// was at least `min_gap_us` ago (a new link is due at once, so the first
    /// sighting is not delayed).
    pub fn take_due(&mut self, now_us: u64, min_gap_us: u64, mut out: impl FnMut(&Sighting)) {
        for s in self.slots.iter_mut().flatten() {
            let fresh = s.packets != s.reported_packets;
            let gap_ok = s
                .reported_us
                .is_none_or(|r| now_us.saturating_sub(r) >= min_gap_us);
            if fresh && gap_ok {
                out(s);
                s.reported_us = Some(now_us);
                s.reported_packets = s.packets;
            }
        }
    }

    /// Links currently in the table.
    pub fn iter(&self) -> impl Iterator<Item = &Sighting> {
        self.slots.iter().flatten()
    }
}
