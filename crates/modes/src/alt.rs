//! Altitude and identity (squawk) codes: the 13-bit AC field of DF0/4/16/20,
//! the 12-bit altitude of ADS-B airborne position messages, and the 13-bit
//! ID field of DF5/21 and ADS-B emergency status.
//!
//! Bit layouts follow ICAO Annex 10 Vol IV 3.1.2.6.5.4 (AC) and
//! 3.1.2.6.7.1 (ID). With the Q bit set the altitude is a plain 11-bit
//! number in 25 ft steps; with it clear it is the Gillham (Mode C) code in
//! 100 ft steps, which older transponders still send.

/// A decoded pressure altitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Altitude {
    /// Feet, referenced to 29.92 inHg.
    Feet(i32),
    /// The M bit says metric. Practically never sent; not decoded.
    Metric,
    /// All zeros: no altitude information.
    Unavailable,
    /// A Gillham code that is not a valid altitude.
    Invalid,
}

impl Altitude {
    pub fn feet(self) -> Option<i32> {
        match self {
            Altitude::Feet(f) => Some(f),
            _ => None,
        }
    }
}

/// The 13-bit AC field: C1 A1 C2 A2 C4 A4 M B1 Q B2 D2 B4 D4 (MSB first).
pub fn ac13(field: u16) -> Altitude {
    let f = field & 0x1FFF;
    if f == 0 {
        return Altitude::Unavailable;
    }
    let m = f & 0x0040 != 0;
    if m {
        return Altitude::Metric;
    }
    let q = f & 0x0010 != 0;
    if q {
        // Drop M (bit 6 from the right) and Q (bit 4): 11 bits, 25 ft.
        let n = ((f & 0x1F80) >> 2) | ((f & 0x0020) >> 1) | (f & 0x000F);
        return Altitude::Feet(i32::from(n) * 25 - 1000);
    }
    gillham(f)
}

/// The 12-bit altitude of an ADS-B airborne position: the AC13 layout
/// with the M bit removed (C1 A1 C2 A2 C4 A4 B1 Q B2 D2 B4 D4).
pub fn ac12(field: u16) -> Altitude {
    let f = field & 0x0FFF;
    // Put the M bit (0) back in and decode as AC13.
    let ac = ((f & 0x0FC0) << 1) | (f & 0x003F);
    ac13(ac)
}

/// Gillham (Mode C) code, given as an AC13 field with Q and M clear.
///
/// The 500 ft part is the reflected Gray code D2 D4 A1 A2 A4 B1 B2 B4 (D1
/// is never used); the 100 ft part is C1 C2 C4, a Gray code over 1..=5 that
/// counts down when the 500 ft part is odd.
fn gillham(f: u16) -> Altitude {
    let bit = |mask: u16| u16::from(f & mask != 0);
    let (c1, a1, c2, a2, c4, a4) = (bit(0x1000), bit(0x0800), bit(0x0400), bit(0x0200), bit(0x0100), bit(0x0080));
    let (b1, b2, d2, b4, d4) = (bit(0x0020), bit(0x0008), bit(0x0004), bit(0x0002), bit(0x0001));
    let gray500 = d2 << 7 | d4 << 6 | a1 << 5 | a2 << 4 | a4 << 3 | b1 << 2 | b2 << 1 | b4;
    let gray100 = c1 << 2 | c2 << 1 | c4;
    let fives = gray_to_binary(gray500);
    let mut hundreds = gray_to_binary(gray100);
    // The 100 ft code takes the Gray values 1, 2, 3, 4 and 7 (for 5):
    // C1 C2 C4 = 001 011 010 110 100. Anything else is not an altitude.
    hundreds = match hundreds {
        1..=4 => hundreds,
        7 => 5,
        _ => return Altitude::Invalid,
    };
    if fives & 1 != 0 {
        hundreds = 6 - hundreds;
    }
    Altitude::Feet(i32::from(fives) * 500 + i32::from(hundreds) * 100 - 1300)
}

fn gray_to_binary(g: u16) -> u16 {
    let mut b = g;
    let mut shift = g >> 1;
    while shift != 0 {
        b ^= shift;
        shift >>= 1;
    }
    b
}

/// The 13-bit identity field: C1 A1 C2 A2 C4 A4 X B1 D1 B2 D2 B4 D4.
/// Returns the squawk as its octal value (`0o7700` for 7700), the same
/// convention as [`adsb::Aircraft::squawk`].
pub fn squawk(field: u16) -> u16 {
    let f = field & 0x1FFF;
    let bit = |mask: u16| u16::from(f & mask != 0);
    let a = bit(0x0080) << 2 | bit(0x0200) << 1 | bit(0x0800); // A4 A2 A1
    let b = bit(0x0002) << 2 | bit(0x0008) << 1 | bit(0x0020); // B4 B2 B1
    let c = bit(0x0100) << 2 | bit(0x0400) << 1 | bit(0x1000); // C4 C2 C1
    let d = bit(0x0001) << 2 | bit(0x0004) << 1 | bit(0x0010); // D4 D2 D1
    a << 9 | b << 6 | c << 3 | d
}

/// The inverse of [`squawk`] (tests and test-frame builders).
pub fn squawk_field(code: u16) -> u16 {
    let a = (code >> 9) & 7;
    let b = (code >> 6) & 7;
    let c = (code >> 3) & 7;
    let d = code & 7;
    let mut f = 0u16;
    let mut set = |cond: u16, mask: u16| {
        if cond != 0 {
            f |= mask;
        }
    };
    set(a & 4, 0x0080);
    set(a & 2, 0x0200);
    set(a & 1, 0x0800);
    set(b & 4, 0x0002);
    set(b & 2, 0x0008);
    set(b & 1, 0x0020);
    set(c & 4, 0x0100);
    set(c & 2, 0x0400);
    set(c & 1, 0x1000);
    set(d & 4, 0x0001);
    set(d & 2, 0x0004);
    set(d & 1, 0x0010);
    f
}

/// Altitude in 25 ft steps as a 12-bit ADS-B field (Q bit set), for test
/// frames. `None` outside -1000..=50175 ft or off the 25 ft grid.
pub fn ac12_field_25ft(feet: i32) -> Option<u16> {
    if (feet + 1000) % 25 != 0 {
        return None;
    }
    let n = u16::try_from((feet + 1000) / 25).ok().filter(|n| *n < 2048)?;
    Some(((n & 0x7F0) << 1) | 0x0010 | (n & 0x000F))
}
