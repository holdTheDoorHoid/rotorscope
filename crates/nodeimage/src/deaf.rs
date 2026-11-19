//! The deaf fall-back (design WP-13, section 7.2).
//!
//! A node relayed through the ESP-NOW gateway hears the hub only while its
//! plan has it on the gateway channel. If a plan from the hub (or one read
//! back from `rsjob`) keeps it away, nothing the hub sends can ever fix it.
//! So: with no accepted (verified) gateway packet for [`DEAF_MS`] while the
//! node runs a hub-given or stored plan (plan id not 0), the node goes back
//! to its role's built-in plan, reports plan id 0, counts the fall-back and
//! refuses that plan id until it reboots.
//!
//! A small state machine with the clock passed in, so it is host-tested;
//! each node image feeds it (`firmware/node-wifi/src/role.rs`).

/// No accepted gateway packet for this long on a hub plan: fall back.
pub const DEAF_MS: u64 = 60_000;
/// Plan ids remembered as refused. More fall-backs than this in one boot
/// forget the oldest (each is still counted).
pub const REFUSED_MAX: usize = 8;

#[derive(Debug, Clone)]
pub struct DeafWatch {
    /// When the last accepted gateway packet arrived (or boot).
    last_heard_ms: u64,
    refused: heapless::Vec<u16, REFUSED_MAX>,
    fallbacks: u32,
}

impl DeafWatch {
    /// At boot: the clock starts now, as if the gateway had just been heard
    /// (a stored plan gets its full minute).
    pub const fn new(now_ms: u64) -> Self {
        Self { last_heard_ms: now_ms, refused: heapless::Vec::new(), fallbacks: 0 }
    }

    /// An accepted (verified) packet from the gateway: beacon, command or
    /// time sync.
    pub fn heard(&mut self, now_ms: u64) {
        self.last_heard_ms = self.last_heard_ms.max(now_ms);
    }

    /// Call at least once a second with the plan id being run (0 = the
    /// built-in plan). `true`: fall back to the built-in plan now; the id
    /// is refused from now on and the fall-back counted.
    pub fn check(&mut self, now_ms: u64, plan_id: u16) -> bool {
        if plan_id == 0 || now_ms.saturating_sub(self.last_heard_ms) < DEAF_MS {
            return false;
        }
        if !self.refuses(plan_id) {
            if self.refused.is_full() {
                self.refused.remove(0);
            }
            let _ = self.refused.push(plan_id);
        }
        self.fallbacks = self.fallbacks.saturating_add(1);
        // A fresh minute for whatever runs next.
        self.last_heard_ms = now_ms;
        true
    }

    /// Whether `plan_id` made this node deaf since boot (refuse it). Never
    /// the built-in plan.
    pub fn refuses(&self, plan_id: u16) -> bool {
        plan_id != 0 && self.refused.contains(&plan_id)
    }

    /// Fall-backs since boot.
    pub const fn fallbacks(&self) -> u32 {
        self.fallbacks
    }

    /// Milliseconds since the last accepted gateway packet.
    pub const fn silent_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.last_heard_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deaf_60s_falls_back() {
        let mut w = DeafWatch::new(1_000);
        // Not before 60 s.
        for t in (1_000..61_000).step_by(1_000) {
            assert!(!w.check(t, 42), "at {t}");
        }
        assert!(!w.check(60_999, 42));
        assert!(w.check(61_000, 42), "at 60 s");
        assert_eq!(w.fallbacks(), 1);
        // The built-in plan never falls back, however long it is deaf.
        assert!(!w.check(10_000_000, 0));
        assert_eq!(w.fallbacks(), 1);
    }

    #[test]
    fn heard_gateway_restarts_the_minute() {
        let mut w = DeafWatch::new(0);
        w.heard(50_000);
        assert!(!w.check(100_000, 7));
        assert!(!w.check(109_999, 7));
        assert!(w.check(110_000, 7));
        // An older time never moves the clock back.
        let mut w = DeafWatch::new(0);
        w.heard(30_000);
        w.heard(10_000);
        assert_eq!(w.silent_ms(40_000), 10_000);
    }

    #[test]
    fn refused_plan_id_stays_refused() {
        let mut w = DeafWatch::new(0);
        assert!(!w.refuses(42));
        assert!(w.check(60_000, 42));
        assert!(w.refuses(42));
        // Hearing the gateway again does not lift it: only a reboot does.
        for t in (60_000..1_000_000).step_by(500) {
            w.heard(t);
        }
        assert!(w.refuses(42));
        assert!(!w.refuses(43));
        assert!(!w.refuses(0), "the built-in plan is never refused");
        // More fall-backs than remembered: the oldest is forgotten, every
        // one is counted.
        let mut t = 1_000_000;
        for id in 100..100 + REFUSED_MAX as u16 {
            t += DEAF_MS;
            assert!(w.check(t, id));
        }
        assert!(!w.refuses(42));
        assert!(w.refuses(100) && w.refuses(100 + REFUSED_MAX as u16 - 1));
        assert_eq!(w.fallbacks(), 1 + REFUSED_MAX as u32);
    }
}
