//! What the hub publishes as its own position and time, given the integrity verdict
//! (docs/GNSS-INTEGRITY.md, "Holding").
//!
//! Generic over the hub's own fix and time-anchor types, so the decision is host-tested here
//! and the firmware only applies it.
//!
//! - A **trusted** epoch ([`crate::IntegrityStatus::trusted`]: clean for
//!   [`crate::integrity::TRUST_EPOCHS`] in a row, no spoofing state) publishes the receiver's
//!   fix and time as they are, and remembers them as the last trusted ones.
//! - Any other epoch publishes the **last trusted** fix and time, marked held, or **nothing**
//!   when nothing was ever trusted this boot. Never the receiver's: an epoch in which a
//!   spoofing check fired, or the first epochs of a spoofer that has not yet tripped a check
//!   for long enough, must not reach the evidence log, the aircraft decoder or the aero nodes.
//! - [`Publisher::forget_time`] drops the trusted time (the floor check found it was before
//!   the build date or the remembered time).

/// One published value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pub<X> {
    /// This epoch's value, trusted.
    Live(X),
    /// The last trusted value, held (the receiver is not believed now).
    Held(X),
    /// Leave what is published as it is (the receiver gave nothing this epoch).
    Keep,
    /// Publish nothing: nothing trusted to hold.
    Withhold,
}

/// What to publish this epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Out<F, T> {
    /// Position.
    pub fix: Pub<F>,
    /// Time.
    pub time: Pub<T>,
}

/// The last trusted fix and time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Publisher<F, T> {
    fix: Option<F>,
    time: Option<T>,
    /// The last output was a hold (so a trusted epoch without a fix still releases it).
    held: bool,
}

impl<F: Copy, T: Copy> Default for Publisher<F, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F: Copy, T: Copy> Publisher<F, T> {
    /// Nothing trusted yet.
    pub const fn new() -> Self {
        Publisher { fix: None, time: None, held: false }
    }

    /// The last trusted fix.
    pub fn trusted_fix(&self) -> Option<F> {
        self.fix
    }

    /// The last trusted time.
    pub fn trusted_time(&self) -> Option<T> {
        self.time
    }

    /// Drop the trusted time.
    pub fn forget_time(&mut self) {
        self.time = None;
    }

    /// One epoch. `trusted`: the integrity monitor trusts this epoch; `fix`, `time`: what the
    /// receiver gave (a usable fix, a vouched-for UTC), if anything.
    pub fn epoch(&mut self, trusted: bool, fix: Option<F>, time: Option<T>) -> Out<F, T> {
        if trusted {
            let was_held = self.held;
            self.held = false;
            let fix = match fix {
                Some(f) => {
                    self.fix = Some(f);
                    Pub::Live(f)
                }
                // Released from a hold with no fix this epoch: the last trusted one, unheld,
                // ages out normally.
                None if was_held => self.fix.map_or(Pub::Withhold, Pub::Live),
                None => Pub::Keep,
            };
            let time = match time {
                Some(t) => {
                    self.time = Some(t);
                    Pub::Live(t)
                }
                None if was_held => self.time.map_or(Pub::Withhold, Pub::Live),
                None => Pub::Keep,
            };
            return Out { fix, time };
        }
        self.held = true;
        Out { fix: self.fix.map_or(Pub::Withhold, Pub::Held), time: self.time.map_or(Pub::Withhold, Pub::Held) }
    }
}
