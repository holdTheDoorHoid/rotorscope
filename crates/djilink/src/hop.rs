//! Confirming a DJI controller from its hops.
//!
//! One narrow, short, 15 kHz OFDM burst ([`crate::Verdict::ControllerBurst`])
//! is already unusual, but a controller is a *hopping* transmitter: its
//! bursts move on a 2 MHz raster across the band (59 channels from 5730.12 to
//! 5846.12 MHz filed for OcuSync 2's "1.4 MHz-CA" mode). The tracker keeps
//! the last few burst centres and confirms the controller when, within
//! [`WINDOW_MS`], bursts have been seen on at least [`MIN_CENTRES`] centres at
//! least [`MIN_STEP_KHZ`] apart. Several bursts of one capture on different
//! centres count too ([`HopTracker::burst`] once per centre).
//!
//! A fixed narrow transmitter (one centre) never confirms; neither does a
//! single burst.

/// Burst centres remembered.
pub const SLOTS: usize = 8;
/// How long a burst counts toward confirmation: two built-in FPV sweeps.
/// The sweep visits 46 channels at 60 ms (plus retuning), about 3 s, and
/// one capture per dwell sees at most a burst or two; the measured O4
/// uplink (RFUAV Mini 4 Pro) changes centre frame to frame (2.5 ms) but
/// stays inside one 5-10 MHz set for hundreds of ms, so its second centre
/// is often seen only on the next sweep. 3 s (the first guess) could not
/// span one revisit.
pub const WINDOW_MS: u32 = 6_000;
/// Distinct centres needed.
pub const MIN_CENTRES: usize = 2;
/// Centres closer than this are the same channel (half the 2 MHz raster).
pub const MIN_STEP_KHZ: u32 = 1_000;

#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    centre_khz: u32,
    t_ms: u32,
}

#[derive(Clone, Debug, Default)]
pub struct HopTracker {
    seen: [Seen; SLOTS],
    n: usize,
    next: usize,
}

impl HopTracker {
    pub const fn new() -> Self {
        HopTracker { seen: [Seen { centre_khz: 0, t_ms: 0 }; SLOTS], n: 0, next: 0 }
    }

    pub fn reset(&mut self) {
        *self = HopTracker::new();
    }

    /// One controller-shaped burst at absolute `centre_khz`, seen at
    /// `now_ms` (any monotonic millisecond clock; wraps after 49 days).
    /// Returns whether the controller is now confirmed.
    pub fn burst(&mut self, centre_khz: u32, now_ms: u32) -> bool {
        self.seen[self.next] = Seen { centre_khz, t_ms: now_ms };
        self.next = (self.next + 1) % SLOTS;
        self.n = (self.n + 1).min(SLOTS);
        self.confirmed(now_ms)
    }

    /// Distinct centres seen within [`WINDOW_MS`] of `now_ms`.
    pub fn centres(&self, now_ms: u32) -> usize {
        let mut distinct = [0u32; SLOTS];
        let mut k = 0;
        for s in &self.seen[..self.n] {
            if now_ms.wrapping_sub(s.t_ms) > WINDOW_MS {
                continue;
            }
            if distinct[..k].iter().all(|&c| c.abs_diff(s.centre_khz) >= MIN_STEP_KHZ) {
                distinct[k] = s.centre_khz;
                k += 1;
            }
        }
        k
    }

    pub fn confirmed(&self, now_ms: u32) -> bool {
        self.centres(now_ms) >= MIN_CENTRES
    }
}
