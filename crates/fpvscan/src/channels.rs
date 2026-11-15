//! The analog FPV channel table and what the C5 can reach.
//!
//! Bands A, B, E, F, R (Raceband) and L, channels 1-8 (docs/SIGNALS.md). The
//! raw-capture C5 is measured from 5180 to 5800 MHz (LO); the capture after
//! decimation is 20 MHz wide, so a carrier up to [`REACH_MAX_MHZ`] is still
//! inside it when the LO sits at the 5800 edge. Each channel is received with
//! its carrier [`OFFSET_MHZ`] above the LO (off the DC spike); above the LO
//! limit the LO stays at 5800 and the carrier moves further up the capture.

/// `(band letter, channel 1..8 centres in MHz)`.
pub const BANDS: [(u8, [u16; 8]); 6] = [
    (b'A', [5865, 5845, 5825, 5805, 5785, 5765, 5745, 5725]),
    (b'B', [5733, 5752, 5771, 5790, 5809, 5828, 5847, 5866]),
    (b'E', [5705, 5685, 5665, 5645, 5885, 5905, 5925, 5945]),
    (b'F', [5740, 5760, 5780, 5800, 5820, 5840, 5860, 5880]),
    (b'R', [5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917]),
    (b'L', [5362, 5399, 5436, 5473, 5510, 5547, 5584, 5621]),
];

/// Lowest and highest LO the raw capture is measured at on 5 GHz.
pub const LO_MIN_MHZ: u16 = 5180;
pub const LO_MAX_MHZ: u16 = 5800;
/// Carrier above the LO when the LO is free to follow it.
pub const OFFSET_MHZ: u16 = 1;
/// Highest carrier the scanner tunes to: 10 MHz above the highest LO, the
/// edge of the 20 MHz capture (the analog low-pass is about +-10 MHz).
pub const REACH_MAX_MHZ: u16 = LO_MAX_MHZ + 10;
pub const REACH_MIN_MHZ: u16 = LO_MIN_MHZ + OFFSET_MHZ;

/// Dwell per channel in the built-in sweep (same as `crates/coverage` FPV_MS).
pub const DWELL_MS: u16 = 60;

pub const fn reachable(mhz: u16) -> bool {
    mhz >= REACH_MIN_MHZ && mhz <= REACH_MAX_MHZ
}

/// The LO for a carrier at `mhz`, and the carrier's offset from it (kHz).
pub const fn lo_for(mhz: u16) -> (u16, i32) {
    let lo = if mhz - OFFSET_MHZ > LO_MAX_MHZ { LO_MAX_MHZ } else { mhz - OFFSET_MHZ };
    (lo, (mhz as i32 - lo as i32) * 1000)
}

/// Band letter and channel number of the first table entry at `mhz`.
pub fn label(mhz: u16) -> Option<(u8, u8)> {
    for (band, chans) in BANDS {
        for (k, &c) in chans.iter().enumerate() {
            if c == mhz {
                return Some((band, k as u8 + 1));
            }
        }
    }
    None
}

/// Every distinct channel centre, sorted (47: R7 and F8 are both 5880).
pub const ALL: ([u16; 48], usize) = {
    let mut out = [0u16; 48];
    let mut n = 0;
    let mut b = 0;
    while b < BANDS.len() {
        let mut k = 0;
        while k < 8 {
            let f = BANDS[b].1[k];
            let mut dup = false;
            let mut j = 0;
            while j < n {
                if out[j] == f {
                    dup = true;
                }
                j += 1;
            }
            if !dup {
                out[n] = f;
                n += 1;
            }
            k += 1;
        }
        b += 1;
    }
    // Insertion sort.
    let mut i = 1;
    while i < n {
        let mut j = i;
        while j > 0 && out[j - 1] > out[j] {
            let t = out[j];
            out[j] = out[j - 1];
            out[j - 1] = t;
            j -= 1;
        }
        i += 1;
    }
    (out, n)
};

/// Every distinct channel the C5 can reach, sorted: the built-in sweep.
pub const SWEEP: ([u16; 48], usize) = {
    let mut out = [0u16; 48];
    let mut n = 0;
    let mut i = 0;
    while i < ALL.1 {
        if reachable(ALL.0[i]) {
            out[n] = ALL.0[i];
            n += 1;
        }
        i += 1;
    }
    (out, n)
};

pub fn all() -> &'static [u16] {
    &ALL.0[..ALL.1]
}

pub fn sweep() -> &'static [u16] {
    &SWEEP.0[..SWEEP.1]
}

/// Channels the C5 cannot reach (above [`REACH_MAX_MHZ`]).
pub fn skipped() -> impl Iterator<Item = u16> {
    all().iter().copied().filter(|&f| !reachable(f))
}

/// HDZero's channel grid: Raceband (37 MHz apart), E1, F1, F2, F4 and the
/// low band (docs/SIGNALS.md), within 2 MHz.
pub fn hdzero_grid(mhz: u16) -> bool {
    let near = |c: u16| mhz.abs_diff(c) <= 2;
    BANDS[4].1.iter().any(|&c| near(c))
        || BANDS[5].1.iter().any(|&c| near(c))
        || [5705, 5740, 5760, 5800].into_iter().any(near)
}
