//! The two transmit slots of every UTC second and what to listen on.
//!
//! FLARM ("Legacy") and OGNTP protocol descriptors in SoftRF
//! (`Legacy.cpp`, `OGNTP.cpp`): `slot0 = {400, 800}`, `slot1 = {800, 1200}`
//! milliseconds after the PPS edge of second `T`. Both slots use second
//! `T`'s channels, so slot 1 runs 200 ms into second `T + 1` still on
//! `T`'s channel. From 200 to 400 ms nobody transmits ([`SlotKind::Gap`]);
//! we tune to the coming slot 0 there.
//!
//! SoftRF's own receiver (`RF_SetChannel`, `RF_TIMING_2SLOTS_PPS_SYNC`)
//! changes channel at 300 ms (`slot0.begin - adj`, `adj = 100`) and 800 ms.
//! Retuning anywhere inside the silent gap is equivalent for a receiver; we
//! retune at its start (200 ms) to leave the most margin for clock error.

use crate::Follow;
use crate::plan::FreqPlan;

/// Slot 0: milliseconds after the second's edge.
pub const SLOT0_MS: (u64, u64) = (400, 800);
/// Slot 1, likewise (ends 200 ms into the next second).
pub const SLOT1_MS: (u64, u64) = (800, 1200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Slot0,
    Slot1,
    /// 200-400 ms: no transmissions; listen on the coming slot 0's channel.
    Gap,
}

/// One stretch of time with one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub kind: SlotKind,
    /// The UTC second (Unix) whose channels apply.
    pub second: u32,
    /// The slot number to pass to `getChannel` (the gap uses slot 0).
    pub slot: u8,
    /// UTC milliseconds, start inclusive, end exclusive.
    pub start_ms: u64,
    pub end_ms: u64,
}

/// The window UTC instant `utc_ms` (Unix milliseconds) falls in.
pub fn window_at(utc_ms: u64) -> Window {
    let s = utc_ms / 1000;
    let m = utc_ms % 1000;
    let base = s * 1000;
    if m < 200 {
        // Slot 1 of the previous second. (Before 1970-01-01T00:00:00.2 there
        // is no previous second; treat it as the gap before slot 0.)
        if s == 0 {
            return Window { kind: SlotKind::Gap, second: 0, slot: 0, start_ms: 0, end_ms: SLOT0_MS.0 };
        }
        let prev = base - 1000;
        Window { kind: SlotKind::Slot1, second: (s - 1) as u32, slot: 1, start_ms: prev + SLOT1_MS.0, end_ms: prev + SLOT1_MS.1 }
    } else if m < SLOT0_MS.0 {
        Window { kind: SlotKind::Gap, second: s as u32, slot: 0, start_ms: base + 200, end_ms: base + SLOT0_MS.0 }
    } else if m < SLOT0_MS.1 {
        Window { kind: SlotKind::Slot0, second: s as u32, slot: 0, start_ms: base + SLOT0_MS.0, end_ms: base + SLOT0_MS.1 }
    } else {
        Window { kind: SlotKind::Slot1, second: s as u32, slot: 1, start_ms: base + SLOT1_MS.0, end_ms: base + SLOT1_MS.1 }
    }
}

/// Where to listen now and until when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listen {
    pub window: Window,
    pub channel: u8,
    pub freq_hz: u32,
}

/// The channel to listen on at `utc_ms`, following `follow`'s hops.
/// Retune when the clock reaches `window.end_ms` (and call again).
pub fn listen(plan: &FreqPlan, utc_ms: u64, follow: Follow) -> Listen {
    let window = window_at(utc_ms);
    let channel = plan.channel(window.second, window.slot, follow.ogn());
    Listen { window, channel, freq_hz: plan.channel_hz(channel) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Band;

    const T: u64 = 1_763_337_600; // 2025-11-17T00:00:00Z

    #[test]
    fn windows_over_one_second() {
        let w = |ms| window_at(T * 1000 + ms);
        assert_eq!(
            w(0),
            Window { kind: SlotKind::Slot1, second: (T - 1) as u32, slot: 1, start_ms: T * 1000 - 200, end_ms: T * 1000 + 200 }
        );
        assert_eq!(w(199).kind, SlotKind::Slot1);
        assert_eq!(w(200), Window { kind: SlotKind::Gap, second: T as u32, slot: 0, start_ms: T * 1000 + 200, end_ms: T * 1000 + 400 });
        assert_eq!(w(399).kind, SlotKind::Gap);
        assert_eq!(w(400), Window { kind: SlotKind::Slot0, second: T as u32, slot: 0, start_ms: T * 1000 + 400, end_ms: T * 1000 + 800 });
        assert_eq!(w(799).kind, SlotKind::Slot0);
        assert_eq!(w(800), Window { kind: SlotKind::Slot1, second: T as u32, slot: 1, start_ms: T * 1000 + 800, end_ms: T * 1000 + 1200 });
        assert_eq!(w(999).second, T as u32);
        assert_eq!(w(1000).second, T as u32, "slot 1 runs into the next second on this second's channel");
        assert_eq!(window_at(5), Window { kind: SlotKind::Gap, second: 0, slot: 0, start_ms: 0, end_ms: 400 });
    }

    /// Windows tile time: each ends where the next begins, every instant is
    /// inside its own window.
    #[test]
    fn windows_tile_time() {
        let mut t = T * 1000 + 37;
        let mut w = window_at(t);
        for _ in 0..1000 {
            assert!(w.start_ms <= t && t < w.end_ms);
            t = w.end_ms;
            let next = window_at(t);
            assert_eq!(next.start_ms, w.end_ms);
            w = next;
        }
    }

    #[test]
    fn listen_follows_flarm_and_ogn() {
        let us = Band::Us.plan();
        let at = |ms, f| listen(&us, ms, f);
        // Second 1763337600: FLARM slot 0 = 36, slot 1 = 35; OGN 37 / 36.
        let s0 = at(T * 1000 + 500, Follow::Flarm);
        assert_eq!((s0.window.kind, s0.channel, s0.freq_hz), (SlotKind::Slot0, 36, 902_200_000 + 36 * 400_000));
        assert_eq!(at(T * 1000 + 900, Follow::Flarm).channel, 35);
        assert_eq!(at(T * 1000 + 1100, Follow::Flarm).channel, 35);
        assert_eq!(at(T * 1000 + 300, Follow::Flarm).channel, 36, "the gap pre-tunes slot 0");
        assert_eq!(at(T * 1000 + 500, Follow::Ogn).channel, 37);
        assert_eq!(at(T * 1000 + 900, Follow::Ogn).channel, 36);
        let eu = Band::Eu.plan();
        assert_eq!(listen(&eu, T * 1000 + 500, Follow::Flarm).freq_hz, 868_200_000);
        assert_eq!(listen(&eu, T * 1000 + 900, Follow::Flarm).freq_hz, 868_400_000);
    }
}
