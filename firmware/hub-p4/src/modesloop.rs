//! Bench only (`--features modesloop`): on-chip loop-back test of our Mode S
//! decoder (`--features modes`, PLAN.md Phase 14a). Nothing outside the chip
//! is driven or needed; **leave GPIO30 unconnected while it runs**.
//!
//! UART4's TX drives its own RX pad (GPIO30), the ADSBee input, with Beast
//! frames recorded from the owner's ADSBee 1090 on the laptop
//! (`tools/modes-live live ... --record`, then `modes-live export`; 2026-11-15,
//! Philadelphia, real aircraft), at their recorded pace. The recording loops;
//! each pass is logged. `modes-live check-export` prints what the host
//! decoder makes of the same file, for comparison with the console here.

use esp_hal::time::Instant;

use crate::uartring::adsb_rx;

/// `recordings/adsbee-2026-11-15.beast`, made with `modes-live export` (our
/// own recording, so it may be committed; `.bin` files are ignored by git).
/// Records: `u32` LE milliseconds since the first frame, one length byte,
/// then the Beast-escaped frame.
static REC: &[u8] = include_bytes!("../recordings/adsbee-2026-11-15.beast");

pub struct Loop {
    pos: usize,
    start: Instant,
    pub passes: u32,
    pub sent: u32,
}

impl Loop {
    pub fn new() -> Self {
        crate::println!("hub-p4: modesloop: replaying {} bytes of recorded Beast frames onto GPIO30 (UART4 TX)", REC.len());
        Self { pos: 0, start: Instant::now(), passes: 0, sent: 0 }
    }

    pub fn tick(&mut self) {
        let elapsed = (Instant::now() - self.start).as_millis();
        // A few frames per call at most, so the loop is never held up long.
        for _ in 0..8 {
            if self.pos + 5 > REC.len() {
                self.passes += 1;
                crate::println!("hub-p4: modesloop: pass {} done, {} frames sent", self.passes, self.sent);
                self.pos = 0;
                self.start = Instant::now();
                return;
            }
            let at = u32::from_le_bytes([REC[self.pos], REC[self.pos + 1], REC[self.pos + 2], REC[self.pos + 3]]);
            if u64::from(at) > elapsed {
                return;
            }
            let n = usize::from(REC[self.pos + 4]);
            let end = (self.pos + 5 + n).min(REC.len());
            adsb_rx::write_all(&REC[self.pos + 5..end]);
            self.sent += 1;
            self.pos = end;
        }
    }
}
