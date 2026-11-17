//! Following FLARM and OGN-tracker frequency hops on a receive-only node.
//!
//! FLARM and OGN trackers in the 915 MHz band (USA/Canada, Australia/South
//! America) hop: every second is split into two transmit slots, and each
//! slot uses a channel picked by a hash of the UTC second. A receiver parked
//! on one channel hears about 1 slot in 65 (US plan). A receiver that knows
//! UTC to well under a slot (400 ms) can be on the right channel every slot.
//! In Europe there is no hopping: two fixed channels, swapped between slots.
//!
//! - [`plan`]: the frequency plans and the channel for a UTC second and slot,
//!   ported exactly from the OGN tracker's `freqplan.h` (as carried in
//!   SoftRF, `libraries/OGN/freqplan.h`), including `FreqHopHash`;
//! - [`slots`]: which slot a UTC instant is in, when it ends and which
//!   frequency to listen on (slot windows from SoftRF's `Legacy.cpp` /
//!   `OGNTP.cpp` protocol descriptors, 400-800 ms and 800-1200 ms after the
//!   second);
//! - [`clock`]: a UTC estimate from the hub's once-a-second time messages,
//!   with drift tracking and a "time unknown" state.
//!
//! Credits: the hop hash and the plans are the OGN tracker project's work
//! (Pawel Jalocha et al., <https://github.com/pjalocha/ogn-tracker>), GPLv3,
//! as redistributed in SoftRF (Linar Yusupov, GPLv3). Rotorscope is GPLv3 as
//! well; the port keeps the arithmetic bit for bit (32-bit wrapping, the
//! `% Channels` on the full hash) so the channels match on air.
//!
//! This crate never decides to transmit anything: it only says where to
//! listen.
#![cfg_attr(not(test), no_std)]

pub mod clock;
pub mod plan;
pub mod slots;

pub use clock::UtcClock;
pub use plan::{Band, FreqPlan};
pub use slots::{Listen, SlotKind, Window};

/// Which hopping sequence to follow. FLARM and OGN trackers share the plan
/// but, in each slot, an OGN tracker sits on a channel next to FLARM's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    Flarm,
    Ogn,
}

impl Follow {
    /// The `OGN` argument of the C `getChannel`.
    pub const fn ogn(self) -> bool {
        matches!(self, Follow::Ogn)
    }
}
