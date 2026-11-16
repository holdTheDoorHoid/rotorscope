//! 2.4 GHz analog video channels and what the C5 can reach there.
//!
//! There is no single 2.4 GHz analog video plan like 5.8 GHz's bands. The
//! two that nearly every transmitter uses (docs/SIGNALS.md, section 3b):
//!
//! - **A**, a 20 MHz grid: 2410, 2430, 2450, 2470, then the out-of-band
//!   2370, 2390, 2490, 2510 (ImmersionRC, LawMate, HobbyKing "MG" 8-channel
//!   modules, FuriousFPV Stealth Long Range band A).
//! - **B**, an 18 MHz grid: 2414, 2432, 2450, 2468, then 2411, 2433, 2453,
//!   2473 (FuriousFPV band B). Its first four are the classic 4-channel
//!   wireless-camera / AV-sender plan (Fat Shark 2.4, CCTV kits, cheap toy
//!   and spy cameras); 8-channel clones add A's 2370/2390/2490/2510.
//!
//! 15 distinct centres; 2370, 2390, 2490 and 2510 lie outside the
//! 2400-2483.5 MHz ISM band, where Wi-Fi and Bluetooth never are.
//!
//! **Reach.** The C5's raw capture is measured at LO 2200-2650 MHz
//! (RESEARCH.md "Can a C5 do it?", rawcap `freq` accepts 2200-2700), so the
//! whole plan and the 2.4 GHz ISM band around it are inside, with room to
//! spare. The carrier sits [`crate::channels::OFFSET_MHZ`] above the LO, as
//! on 5.8 GHz.

use crate::channels::OFFSET_MHZ;

/// `(band letter, channel 1..8 centres in MHz)`, FuriousFPV's order.
pub const BANDS: [(u8, [u16; 8]); 2] = [
    (b'A', [2410, 2430, 2450, 2470, 2370, 2390, 2490, 2510]),
    (b'B', [2414, 2432, 2450, 2468, 2411, 2433, 2453, 2473]),
];

/// Lowest and highest LO the raw capture is measured at on 2.4 GHz.
pub const LO_MIN_MHZ: u16 = 2200;
pub const LO_MAX_MHZ: u16 = 2650;
pub const REACH_MIN_MHZ: u16 = LO_MIN_MHZ + OFFSET_MHZ;
pub const REACH_MAX_MHZ: u16 = LO_MAX_MHZ + OFFSET_MHZ;

/// The ISM band (Wi-Fi channels 1-13 and Bluetooth live inside it).
pub const ISM_MHZ: core::ops::RangeInclusive<u16> = 2400..=2483;

pub const fn reachable(mhz: u16) -> bool {
    mhz >= REACH_MIN_MHZ && mhz <= REACH_MAX_MHZ
}

/// The LO for a carrier at `mhz` (reachable), and the carrier's offset (kHz).
pub const fn lo_for(mhz: u16) -> (u16, i32) {
    (mhz - OFFSET_MHZ, OFFSET_MHZ as i32 * 1000)
}

/// Band letter and channel number of the first table entry at `mhz` (2450
/// is A3 and B3).
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

/// Every distinct centre, sorted.
pub const ALL: ([u16; 16], usize) = {
    let mut out = [0u16; 16];
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

/// Every distinct channel, sorted. All of them are reachable (tested).
pub fn all() -> &'static [u16] {
    &ALL.0[..ALL.1]
}

/// The built-in 2.4 GHz sweep (the whole table).
pub fn sweep() -> &'static [u16] {
    all()
}
