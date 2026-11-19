//! The deaf fall-back (design WP-13, section 7.2).
//!
//! A node relayed through the ESP-NOW gateway hears the hub only while its
//! plan has it on the gateway channel. If a plan from the hub (or one read
//! back from `rsjob`) keeps it away, nothing the hub sends can ever fix it.
//! So, with no accepted (verified) gateway packet for [`DEAF_MS`] while the
//! node runs a hub-given or stored plan (plan id not 0), the node goes back
//! to its role's built-in plan (which always visits the gateway channel)
//! and reports plan id 0.
//!
//! Silence alone does not say whose fault it was: the plan, or a gateway or
//! hub that was away. The built-in plan tells them apart:
//!
//! - **Blamed**: a gateway packet is accepted within [`PROBATION_MS`] of the
//!   fall-back. The gateway was there all along and the plan kept the node
//!   from hearing it. That plan id is refused for a while: [`FIRST_REFUSAL_MS`]
//!   (10 min) the first time, doubling each time the same id is blamed
//!   again, at most [`MAX_REFUSAL_MS`] (1 h). When the refusal ends the hub's
//!   next resend of the plan is tried again.
//! - **Outage**: nothing within [`PROBATION_MS`], on a plan that visits the
//!   gateway channel. The gateway was what was missing: the plan is not
//!   blamed, and the next Assign of it is taken as normal.
//!
//! Nothing is refused for good. The blamed ids live in a table of
//! [`BLAMED_MAX`] entries; when it is full, an entry whose refusal is over
//! makes room first (the one that ended longest ago), else the refusal that
//! ends soonest is ended now. A full table therefore refuses exactly the
//! ids it holds, never everything and never nothing.
//!
//! A small state machine with the clock passed in, so it is host-tested;
//! each node image feeds it (`firmware/node-wifi/src/role.rs`).

/// No accepted gateway packet for this long on a hub plan: fall back.
pub const DEAF_MS: u64 = 60_000;
/// After a fall-back: a gateway packet within this long blames the plan;
/// none means the gateway was away.
pub const PROBATION_MS: u64 = 30_000;
/// First refusal of a blamed plan id; doubles with every further blame.
pub const FIRST_REFUSAL_MS: u64 = 10 * 60_000;
/// Longest refusal.
pub const MAX_REFUSAL_MS: u64 = 60 * 60_000;
/// Plan ids remembered as blamed.
pub const BLAMED_MAX: usize = 8;

/// Something the node should say on USB (and count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Go back to the built-in plan now: `plan` left the node deaf for
    /// [`DEAF_MS`]. On probation for [`PROBATION_MS`].
    FellBack { plan: u16 },
    /// The gateway came back within the probation: `plan` is refused for
    /// `refuse_ms`.
    Blamed { plan: u16, refuse_ms: u64 },
    /// No gateway within the probation either: it was away, `plan` is not
    /// blamed.
    Outage { plan: u16 },
}

/// The node's deaf counters since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub fallbacks: u32,
    pub blamed: u32,
    pub outages: u32,
    /// Assigns turned away because their plan id was under refusal.
    pub refused: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Blame {
    plan: u16,
    /// Times blamed (the refusal is `FIRST_REFUSAL_MS << (strikes - 1)`).
    strikes: u8,
    until_ms: u64,
}

#[derive(Debug, Clone)]
pub struct DeafWatch {
    /// When the last accepted gateway packet arrived (or boot).
    last_heard_ms: u64,
    /// The plan that fell back, and when.
    probation: Option<(u16, u64)>,
    blamed: heapless::Vec<Blame, BLAMED_MAX>,
    counts: Counts,
}

/// Refusal after `strikes` blames.
pub const fn refusal_ms(strikes: u8) -> u64 {
    let shift = if strikes == 0 { 0 } else if strikes > 8 { 7 } else { strikes - 1 };
    let t = FIRST_REFUSAL_MS << shift;
    if t > MAX_REFUSAL_MS { MAX_REFUSAL_MS } else { t }
}

impl DeafWatch {
    /// At boot: the clock starts now, as if the gateway had just been heard
    /// (a stored plan gets its full minute).
    pub const fn new(now_ms: u64) -> Self {
        Self { last_heard_ms: now_ms, probation: None, blamed: heapless::Vec::new(), counts: Counts { fallbacks: 0, blamed: 0, outages: 0, refused: 0 } }
    }

    /// An accepted (verified) packet from the gateway: beacon, command or
    /// time sync. Within the probation it blames the plan that fell back.
    pub fn heard(&mut self, now_ms: u64) -> Option<Event> {
        let ended = self.end_probation(now_ms);
        self.last_heard_ms = self.last_heard_ms.max(now_ms);
        if ended.is_some() {
            return ended;
        }
        let (plan, _) = self.probation.take()?;
        Some(self.blame(plan, now_ms))
    }

    /// Call at least once a second with the plan id being run (0 = the
    /// built-in plan). [`Event::FellBack`]: switch to the built-in plan now.
    pub fn check(&mut self, now_ms: u64, plan_id: u16) -> Option<Event> {
        if let Some(e) = self.end_probation(now_ms) {
            return Some(e);
        }
        if plan_id == 0 || now_ms.saturating_sub(self.last_heard_ms) < DEAF_MS {
            return None;
        }
        self.counts.fallbacks = self.counts.fallbacks.saturating_add(1);
        self.probation = Some((plan_id, now_ms));
        // A fresh minute for whatever runs next.
        self.last_heard_ms = now_ms;
        Some(Event::FellBack { plan: plan_id })
    }

    /// Whether an Assign of `plan_id` must be turned away now (blamed and
    /// still refused). Never the built-in plan.
    pub fn refuses(&self, now_ms: u64, plan_id: u16) -> bool {
        plan_id != 0 && self.blamed.iter().any(|b| b.plan == plan_id && now_ms < b.until_ms)
    }

    /// [`Self::refuses`], counting the refused Assign.
    pub fn refuse_assign(&mut self, now_ms: u64, plan_id: u16) -> bool {
        let r = self.refuses(now_ms, plan_id);
        if r {
            self.counts.refused = self.counts.refused.saturating_add(1);
        }
        r
    }

    /// Milliseconds left on `plan_id`'s refusal (0: not refused).
    pub fn refused_for_ms(&self, now_ms: u64, plan_id: u16) -> u64 {
        self.blamed.iter().find(|b| b.plan == plan_id).map_or(0, |b| b.until_ms.saturating_sub(now_ms))
    }

    pub const fn counts(&self) -> Counts {
        self.counts
    }

    /// Milliseconds since the last accepted gateway packet.
    pub const fn silent_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.last_heard_ms)
    }

    /// The probation ran out with nothing heard: an outage.
    fn end_probation(&mut self, now_ms: u64) -> Option<Event> {
        let (plan, since) = self.probation?;
        if now_ms.saturating_sub(since) <= PROBATION_MS {
            return None;
        }
        self.probation = None;
        self.counts.outages = self.counts.outages.saturating_add(1);
        Some(Event::Outage { plan })
    }

    fn blame(&mut self, plan: u16, now_ms: u64) -> Event {
        self.counts.blamed = self.counts.blamed.saturating_add(1);
        let i = match self.blamed.iter().position(|b| b.plan == plan) {
            Some(i) => i,
            None => {
                if self.blamed.is_full() {
                    // Room: a refusal that is over (ended longest ago),
                    // else the one that ends soonest.
                    let over = (0..self.blamed.len()).filter(|&i| self.blamed[i].until_ms <= now_ms).min_by_key(|&i| self.blamed[i].until_ms);
                    let soonest = (0..self.blamed.len()).min_by_key(|&i| self.blamed[i].until_ms);
                    if let Some(i) = over.or(soonest) {
                        self.blamed.swap_remove(i);
                    }
                }
                let _ = self.blamed.push(Blame { plan, strikes: 0, until_ms: 0 });
                self.blamed.len() - 1
            }
        };
        let b = &mut self.blamed[i];
        b.strikes = b.strikes.saturating_add(1);
        let refuse_ms = refusal_ms(b.strikes);
        b.until_ms = now_ms + refuse_ms;
        Event::Blamed { plan, refuse_ms }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;

    /// Run `plan` from `t` with the gateway silent until `back` (then heard
    /// every second until `until`), checking once a second. Returns the
    /// events in order with their times.
    fn run(w: &mut DeafWatch, plan: &mut u16, t: u64, back: u64, until: u64) -> std::vec::Vec<(u64, Event)> {
        let mut out = std::vec::Vec::new();
        let mut now = t;
        while now <= until {
            if now >= back {
                if let Some(e) = w.heard(now) {
                    out.push((now, e));
                }
            }
            if let Some(e) = w.check(now, *plan) {
                if matches!(e, Event::FellBack { .. }) {
                    *plan = 0;
                }
                out.push((now, e));
            }
            now += 1000;
        }
        out
    }

    #[test]
    fn deaf_60s_falls_back() {
        let mut w = DeafWatch::new(1_000);
        for t in (1_000..61_000).step_by(1_000) {
            assert_eq!(w.check(t, 42), None, "at {t}");
        }
        assert_eq!(w.check(60_999, 42), None);
        assert_eq!(w.check(61_000, 42), Some(Event::FellBack { plan: 42 }), "at 60 s");
        assert_eq!(w.counts().fallbacks, 1);
        // The built-in plan never falls back, however long it is deaf.
        let mut w = DeafWatch::new(0);
        assert_eq!(w.check(10_000_000, 0), None);
        assert_eq!(w.counts(), Counts::default());
    }

    #[test]
    fn heard_gateway_restarts_the_minute() {
        let mut w = DeafWatch::new(0);
        assert_eq!(w.heard(50_000), None);
        assert_eq!(w.check(109_999, 7), None);
        assert_eq!(w.check(110_000, 7), Some(Event::FellBack { plan: 7 }));
        let mut w = DeafWatch::new(0);
        w.heard(30_000);
        w.heard(10_000);
        assert_eq!(w.silent_ms(40_000), 10_000, "an older time never moves the clock back");
    }

    /// Gateway away for 75 s: fall-back at 60 s, gateway back at 75 s
    /// (within the probation): the plan is blamed, refused 10 minutes, then
    /// taken again.
    #[test]
    fn outage_of_75_s_blames_for_10_min() {
        let mut w = DeafWatch::new(0);
        let mut plan = 42;
        let ev = run(&mut w, &mut plan, 0, 75_000, 80_000);
        assert_eq!(ev, [(60_000, Event::FellBack { plan: 42 }), (75_000, Event::Blamed { plan: 42, refuse_ms: 10 * MIN })]);
        assert!(w.refuse_assign(75_000, 42));
        assert!(w.refuse_assign(75_000 + 10 * MIN - 1, 42), "still refused just before 10 min");
        assert!(!w.refuse_assign(75_000 + 10 * MIN, 42), "accepted at 10 min");
        assert!(!w.refuses(75_000, 43) && !w.refuses(75_000, 0), "only that id");
        assert_eq!(w.counts(), Counts { fallbacks: 1, blamed: 1, outages: 0, refused: 2 });
    }

    /// Gateway away for 5 minutes: fall-back at 60 s, nothing in the
    /// probation: an outage, the plan is not blamed and is accepted as soon
    /// as the gateway is back.
    #[test]
    fn outage_of_5_min_is_not_blamed() {
        let mut w = DeafWatch::new(0);
        let mut plan = 42;
        let ev = run(&mut w, &mut plan, 0, 5 * MIN, 5 * MIN + 10_000);
        assert_eq!(ev, [(60_000, Event::FellBack { plan: 42 }), (91_000, Event::Outage { plan: 42 })]);
        // Back at 5 min: the hub's resend is taken.
        assert!(!w.refuse_assign(5 * MIN, 42));
        assert_eq!(w.counts(), Counts { fallbacks: 1, blamed: 0, outages: 1, refused: 0 });
        // On the built-in plan the node does not fall back again, however
        // long the gateway stays away.
        let mut plan = 0;
        assert!(run(&mut w, &mut plan, 5 * MIN, u64::MAX, 60 * MIN).is_empty());
    }

    /// The probation boundary: heard at exactly 30 s blames, later does not.
    #[test]
    fn probation_is_30_s() {
        let mut w = DeafWatch::new(0);
        assert_eq!(w.check(60_000, 5), Some(Event::FellBack { plan: 5 }));
        assert_eq!(w.heard(90_000), Some(Event::Blamed { plan: 5, refuse_ms: 10 * MIN }));
        let mut w = DeafWatch::new(0);
        assert_eq!(w.check(60_000, 5), Some(Event::FellBack { plan: 5 }));
        assert_eq!(w.heard(90_001), Some(Event::Outage { plan: 5 }));
        assert_eq!(w.heard(91_000), None);
        assert!(!w.refuses(91_000, 5));
    }

    /// A plan that really is deaf (the gateway is heard as soon as the
    /// node is back on the built-in plan): refused 10, 20, 40, 60, 60 min.
    #[test]
    fn deafening_plan_refusals_double_to_the_cap() {
        let mut w = DeafWatch::new(0);
        let mut now = 0;
        for (n, want) in [10, 20, 40, 60, 60, 60].into_iter().enumerate() {
            // Taken (not refused), runs deaf for a minute.
            assert!(!w.refuse_assign(now, 9), "try {n}");
            let mut plan = 9;
            let ev = run(&mut w, &mut plan, now, now + DEAF_MS + 2_000, now + DEAF_MS + 3_000);
            assert_eq!(ev, [(now + DEAF_MS, Event::FellBack { plan: 9 }), (now + DEAF_MS + 2_000, Event::Blamed { plan: 9, refuse_ms: want * MIN })], "try {n}");
            let blamed_at = now + DEAF_MS + 2_000;
            assert_eq!(w.refused_for_ms(blamed_at, 9), want * MIN);
            assert!(w.refuses(blamed_at + want * MIN - 1, 9));
            assert!(!w.refuses(blamed_at + want * MIN, 9));
            now = blamed_at + want * MIN;
            w.heard(now);
        }
        assert_eq!(w.counts().blamed, 6);
        assert_eq!(w.counts().fallbacks, 6);
        assert_eq!(refusal_ms(u8::MAX), MAX_REFUSAL_MS);
    }

    /// A full table makes room without accepting or refusing everything.
    #[test]
    fn full_table_stays_bounded() {
        let mut w = DeafWatch::new(0);
        let mut now = 0;
        let blame = |w: &mut DeafWatch, now: &mut u64, id: u16| {
            *now += DEAF_MS;
            assert_eq!(w.check(*now, id), Some(Event::FellBack { plan: id }));
            *now += 1_000;
            assert!(matches!(w.heard(*now), Some(Event::Blamed { plan, .. }) if plan == id));
        };
        for id in 1..=BLAMED_MAX as u16 {
            blame(&mut w, &mut now, id);
        }
        // All eight refused now; id 1 ends soonest.
        assert!((1..=BLAMED_MAX as u16).all(|id| w.refuses(now, id)));
        blame(&mut w, &mut now, 100);
        assert!(w.refuses(now, 100), "the newcomer is refused");
        assert!(!w.refuses(now, 1), "the soonest-ending refusal made room");
        assert!((2..=BLAMED_MAX as u16).all(|id| w.refuses(now, id)), "the others still refused");
        assert!(!w.refuses(now, 555), "an unknown id is taken");
        // Once some refusals are over, they make room first.
        let later = now + 2 * MAX_REFUSAL_MS;
        let mut now = later;
        w.heard(now);
        blame(&mut w, &mut now, 200);
        assert!(w.refuses(now, 200));
        assert_eq!(w.counts().blamed, BLAMED_MAX as u32 + 2);
    }
}
