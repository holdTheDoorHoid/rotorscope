//! The aero node's decisions, as pure logic the firmware calls with the
//! current time: which mode the plan asks for now, where to listen while
//! hopping, when the 1090 receiver needs a restart, and whether a frame may
//! go out under the rate limit.

use aerohop::slots;
use aerohop::{Band, Follow};
use heapless::Vec;
use link::AeroMode;

/// Walks an [`link::Assignment`] plan of [`AeroMode`] entries.
#[derive(Debug, Clone)]
pub struct PlanCursor {
    entries: Vec<(AeroMode, u16), 24>,
    idx: usize,
    since_ms: u64,
    /// Entries skipped because this firmware does not know their code.
    pub unknown: u8,
}

/// The built-in plan: 1090 MHz, for good.
pub const DEFAULT_MODE: AeroMode = AeroMode::Es1090;

impl PlanCursor {
    /// `plan`: the assignment's `(code, dwell_ms)` entries. Unknown codes
    /// (a newer hub's mode) and hopping entries for an unknown band are
    /// skipped; nothing usable left = the built-in plan.
    pub fn new(plan: &[(u32, u16)], now_ms: u64) -> Self {
        let mut entries = Vec::new();
        let mut unknown = 0u8;
        for &(code, dwell) in plan {
            match AeroMode::from_code(code) {
                Some(m @ AeroMode::FlarmHop { band, .. }) if Band::from_number(band).is_some() => {
                    let _ = entries.push((m, dwell));
                }
                Some(m @ (AeroMode::Es1090 | AeroMode::Uat978)) => {
                    let _ = entries.push((m, dwell));
                }
                _ => unknown = unknown.saturating_add(1),
            }
        }
        if entries.is_empty() {
            let _ = entries.push((DEFAULT_MODE, 0));
        }
        Self { entries, idx: 0, since_ms: now_ms, unknown }
    }

    pub fn current(&self) -> AeroMode {
        self.entries[self.idx].0
    }

    /// When the current entry ends, if it does.
    pub fn ends_ms(&self) -> Option<u64> {
        let dwell = self.entries[self.idx].1;
        (self.entries.len() > 1 && dwell > 0).then(|| self.since_ms + u64::from(dwell))
    }

    /// Move on if the dwell is over. True when the mode may have changed.
    pub fn poll(&mut self, now_ms: u64) -> bool {
        match self.ends_ms() {
            Some(end) if now_ms >= end => {
                self.idx = (self.idx + 1) % self.entries.len();
                self.since_ms = now_ms;
                true
            }
            _ => false,
        }
    }
}

/// Where a hopping mode should listen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HopTarget {
    pub freq_hz: u32,
    /// The plan hops but the time is unknown: parked on one channel.
    pub parked: bool,
    /// UTC ms at which to ask again (end of the slot window); `None` when
    /// parked or the plan has a single channel.
    pub until_utc_ms: Option<u64>,
}

pub fn hop_target(band: u8, ogn: bool, utc_ms: Option<u64>) -> Option<HopTarget> {
    let plan = Band::from_number(band)?.plan();
    let follow = if ogn { Follow::Ogn } else { Follow::Flarm };
    if plan.channels <= 1 {
        return Some(HopTarget { freq_hz: plan.channel_hz(0), parked: false, until_utc_ms: None });
    }
    Some(match utc_ms {
        Some(t) => {
            let l = slots::listen(&plan, t, follow);
            HopTarget { freq_hz: l.freq_hz, parked: false, until_utc_ms: Some(l.window.end_ms) }
        }
        None => HopTarget { freq_hz: plan.channel_hz(plan.park_channel()), parked: true, until_utc_ms: None },
    })
}

/// The 1090 MHz receive watchdog, ported from SoftRF `lr20xx_receive`
/// (`ES1090_MONITOR_INTERVAL`): every 30 s, if nothing was received in the
/// last 15 s, restart reception. (SoftRF found the LR20xx OOK receiver can
/// stop delivering packets; a restart brings it back.)
#[derive(Debug, Clone, Default)]
pub struct Es1090Watchdog {
    monitor_ms: u64,
    last_rx_ms: u64,
    pub restarts: u32,
}

pub const ES1090_MONITOR_INTERVAL_MS: u64 = 30_000;

impl Es1090Watchdog {
    pub const fn new(now_ms: u64) -> Self {
        Self { monitor_ms: now_ms, last_rx_ms: now_ms, restarts: 0 }
    }

    /// A packet arrived (any packet the radio delivered, as SoftRF counts it).
    pub fn on_rx(&mut self, now_ms: u64) {
        self.last_rx_ms = now_ms;
    }

    /// Call regularly while receiving; true = restart reception now.
    pub fn poll(&mut self, now_ms: u64) -> bool {
        if now_ms.saturating_sub(self.monitor_ms) <= ES1090_MONITOR_INTERVAL_MS {
            return false;
        }
        self.monitor_ms = now_ms;
        if now_ms.saturating_sub(self.last_rx_ms) > ES1090_MONITOR_INTERVAL_MS / 2 {
            self.restarts = self.restarts.wrapping_add(1);
            return true;
        }
        false
    }
}

/// Token bucket for `Frame::Air`: at most `per_s` a second on average,
/// bursts of up to `per_s`.
#[derive(Debug, Clone)]
pub struct RateLimit {
    per_s: u32,
    /// Tokens x 1e6.
    micro: u64,
    last_us: u64,
    pub dropped: u32,
}

/// The node's limit: 200 frames a second (a busy 1090 site delivers a few
/// hundred DF17 a second to a good receiver; ESP-NOW carries ~12 per batch).
pub const AIR_PER_S: u32 = 200;

impl RateLimit {
    pub const fn new(per_s: u32, now_us: u64) -> Self {
        Self { per_s, micro: per_s as u64 * 1_000_000, last_us: now_us, dropped: 0 }
    }

    pub fn allow(&mut self, now_us: u64) -> bool {
        let cap = self.per_s as u64 * 1_000_000;
        let dt = now_us.saturating_sub(self.last_us);
        self.last_us = now_us;
        self.micro = (self.micro + dt * self.per_s as u64).min(cap);
        if self.micro >= 1_000_000 {
            self.micro -= 1_000_000;
            true
        } else {
            self.dropped = self.dropped.wrapping_add(1);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_cursor() {
        let c = PlanCursor::new(&[], 0);
        assert_eq!(c.current(), AeroMode::Es1090);
        assert_eq!(c.ends_ms(), None);
        let plan = [
            AeroMode::Es1090.entry(5000),
            (12345, 100),
            AeroMode::Uat978.entry(2000),
            AeroMode::FlarmHop { band: 99, ogn: false }.entry(1),
        ];
        let mut c = PlanCursor::new(&plan, 1000);
        assert_eq!(c.unknown, 2);
        assert_eq!(c.current(), AeroMode::Es1090);
        assert!(!c.poll(5999));
        assert!(c.poll(6000));
        assert_eq!(c.current(), AeroMode::Uat978);
        assert!(c.poll(8000));
        assert_eq!(c.current(), AeroMode::Es1090);
        // One entry with a dwell: stays.
        let mut c = PlanCursor::new(&[AeroMode::FlarmHop { band: 2, ogn: true }.entry(100)], 0);
        assert!(!c.poll(1_000_000));
        assert_eq!(c.current(), AeroMode::FlarmHop { band: 2, ogn: true });
    }

    #[test]
    fn hop_targets() {
        let t = 1_763_337_600_000u64;
        let h = hop_target(2, false, Some(t + 500)).unwrap();
        assert_eq!(h, HopTarget { freq_hz: 902_200_000 + 36 * 400_000, parked: false, until_utc_ms: Some(t + 800) });
        let p = hop_target(2, false, None).unwrap();
        assert_eq!(p, HopTarget { freq_hz: 902_200_000 + 32 * 400_000, parked: true, until_utc_ms: None });
        assert_eq!(hop_target(4, false, None).unwrap(), HopTarget { freq_hz: 869_250_000, parked: false, until_utc_ms: None });
        assert!(hop_target(1, false, None).unwrap().parked, "Europe alternates by slot: needs time too");
        assert_eq!(hop_target(42, false, None), None);
    }

    #[test]
    fn watchdog_as_softrf() {
        let mut w = Es1090Watchdog::new(0);
        assert!(!w.poll(30_000));
        // Quiet for > 30 s: restart at the first check after 30 s.
        assert!(w.poll(30_001));
        assert_eq!(w.restarts, 1);
        // Traffic within the last 15 s: no restart.
        w.on_rx(50_000);
        assert!(!w.poll(60_002));
        // Then silence: next check 30 s later finds 40 s quiet.
        assert!(w.poll(90_003));
        assert_eq!(w.restarts, 2);
    }

    #[test]
    fn rate_limit() {
        let mut r = RateLimit::new(200, 0);
        let sent = (0..1000).filter(|_| r.allow(0)).count();
        assert_eq!(sent, 200);
        assert_eq!(r.dropped, 800);
        // One second later: 200 more.
        let sent = (0..1000).filter(|_| r.allow(1_000_000)).count();
        assert_eq!(sent, 200);
        // Steady 150/s is never limited.
        let mut r = RateLimit::new(200, 0);
        for i in 0..1500u64 {
            assert!(r.allow(i * 1_000_000 / 150));
        }
    }
}
