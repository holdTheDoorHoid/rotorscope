//! Satellites one by one (number, band, elevation, azimuth, C/N0), for the integrity monitor.
//!
//! [`crate::sky`] keeps counts per constellation; this keeps the individual satellites of the
//! last complete epoch, from either source:
//!
//! * **UBX-NAV-SAT** (u-blox; class 0x01 id 0x35, M10 SPG 5.10 interface description
//!   UBX-21035062 R03, section 3.15.13.1): one message is a whole epoch and replaces the table.
//! * **NMEA GSV** (LC29H, or any NMEA receiver): satellites are collected from every GSV of an
//!   epoch and the table is published when the epoch's fix is emitted. GSV lists one entry per
//!   satellite **per signal**, so a dual-band receiver lists an L1 and an L5 entry for the same
//!   satellite.
//!
//! Each entry carries a `plausible` flag: false when the satellite number is outside the
//! range its constellation can have, or the elevation/azimuth is outside the sky. Ranges
//! accepted (NMEA 0183 4.10/4.11 as tabulated in UBX-21035062 R03 section 1.5.3 table 3, plus
//! u-blox "extended" numbers and the LC29H's own, LC29H protocol specification V1.3
//! appendix B table 11):
//!
//! | System | NMEA numbers accepted | UBX `svId` accepted |
//! |---|---|---|
//! | GPS (`GP`) | 1-32, SBAS 33-64, QZSS 193-202 | 1-32 (SBAS `gnssId` 1: 120-158) |
//! | GLONASS (`GL`) | 65-96 | 1-32, 255 (slot unknown) |
//! | Galileo (`GA`) | 1-36, 301-336 | 1-36 |
//! | BeiDou (`GB`/`BD`) | 1-63, 201-264, 401-437 | 1-63 |
//! | QZSS (`GQ`) | 1-10, 193-202 | 1-10 |
//! | NavIC (`GI`) | 1-14 | 1-14 |

use crate::sky::{Band, System};

/// Most satellites kept per epoch (signals, on NMEA). More are counted in
/// [`SatTable::overflow`].
pub const MAX_SATS: usize = 64;

/// One satellite (one signal, on NMEA) as the receiver reported it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sat {
    /// Constellation (SBAS counts as GPS).
    pub sys: System,
    /// Satellite number as reported (NMEA satellite ID, or UBX `svId`).
    pub prn: u16,
    /// Frequency band of this entry (UBX NAV-SAT: always L1 on the single-band M10).
    pub band: Band,
    /// Elevation, degrees; `None` when not reported.
    pub elev: Option<i8>,
    /// Azimuth, degrees true; `None` when not reported.
    pub azim: Option<u16>,
    /// Carrier-to-noise density, dB-Hz; `None` when the signal is not tracked.
    pub cn0: Option<u8>,
    /// Used in the navigation solution (UBX only; always false from NMEA).
    pub used: bool,
    /// The number, elevation and azimuth are possible for this constellation.
    pub plausible: bool,
}

const EMPTY: Sat = Sat {
    sys: System::Gps,
    prn: 0,
    band: Band::L1,
    elev: None,
    azim: None,
    cn0: None,
    used: false,
    plausible: true,
};

/// The satellites of the last complete epoch. Read it with [`crate::Parser::sats`].
#[derive(Clone, Copy)]
pub struct SatTable {
    sats: [Sat; MAX_SATS],
    len: u8,
    /// Entries that did not fit.
    pub overflow: u8,
    /// Bumped every time a new epoch is published (0 = never); compare to see fresh data.
    pub seq: u32,
}

impl Default for SatTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SatTable {
    /// Empty.
    pub const fn new() -> Self {
        SatTable { sats: [EMPTY; MAX_SATS], len: 0, overflow: 0, seq: 0 }
    }

    /// The satellites.
    pub fn sats(&self) -> &[Sat] {
        &self.sats[..usize::from(self.len)]
    }

    fn clear(&mut self) {
        self.len = 0;
        self.overflow = 0;
    }

    fn push(&mut self, s: Sat) {
        if usize::from(self.len) < MAX_SATS {
            self.sats[usize::from(self.len)] = s;
            self.len += 1;
        } else {
            self.overflow = self.overflow.saturating_add(1);
        }
    }

    /// Replace the table with a UBX-NAV-SAT payload. Returns false if the payload is malformed
    /// (the table is then left as it was).
    pub(crate) fn set_from_nav_sat(&mut self, p: &[u8]) -> bool {
        if p.len() < 8 || p[4] != 1 {
            return false;
        }
        let n = usize::from(p[5]);
        if p.len() < 8 + 12 * n {
            return false;
        }
        self.clear();
        for i in 0..n {
            let b = &p[8 + 12 * i..20 + 12 * i];
            let (gnss_id, sv) = (b[0], u16::from(b[1]));
            let (sys, plausible) = match gnss_id {
                0 => (System::Gps, (1..=32).contains(&sv)),
                1 => (System::Gps, (120..=158).contains(&sv)),
                2 => (System::Galileo, (1..=36).contains(&sv)),
                3 => (System::Beidou, (1..=63).contains(&sv)),
                5 => (System::Qzss, (1..=10).contains(&sv)),
                6 => (System::Glonass, (1..=32).contains(&sv) || sv == 255),
                7 => (System::Navic, (1..=14).contains(&sv)),
                // 4 = IMES (indoor beacons), anything else unknown: not a sky satellite.
                _ => continue,
            };
            let elev = b[3] as i8;
            let azim = i16::from_le_bytes([b[4], b[5]]);
            let flags = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
            // NAV-SAT: elevation outside +-90 and azimuth outside 0..360 mean "unknown".
            let elev = (-90..=90).contains(&elev).then_some(elev);
            let azim = (0..=360).contains(&azim).then_some(azim as u16);
            let quality = flags & 0x7;
            let cn0 = (b[2] > 0 && quality >= 2).then_some(b[2]);
            self.push(Sat {
                sys,
                prn: sv,
                band: Band::L1,
                elev,
                azim,
                cn0,
                used: flags & 0x08 != 0,
                plausible,
            });
        }
        self.seq = self.seq.wrapping_add(1).max(1);
        true
    }

    /// Statistics of the tracked entries on `band` (L1 is the one every receiver has).
    pub fn cn0_stats(&self, band: Band) -> Cn0Stats {
        let mut st = Cn0Stats::default();
        let mut sum = 0u32;
        let mut sum2 = 0u32;
        for s in self.sats().iter().filter(|s| s.band == band) {
            st.listed += 1;
            if !s.plausible {
                st.implausible += 1;
            }
            let Some(c) = s.cn0 else { continue };
            st.tracked += 1;
            sum += u32::from(c);
            sum2 += u32::from(c) * u32::from(c);
            st.max = st.max.max(c);
            st.min = if st.tracked == 1 { c } else { st.min.min(c) };
            if c >= crate::integrity::CN0_TOO_STRONG {
                st.strong += 1;
            }
        }
        if st.tracked > 0 {
            let n = f32::from(st.tracked);
            st.mean = sum as f32 / n;
            st.var = (sum2 as f32 / n - st.mean * st.mean).max(0.0);
            // Mean of the four strongest: robust to satellites setting into the trees.
            let mut all = [0u8; MAX_SATS];
            let mut k = 0;
            for s in self.sats().iter().filter(|s| s.band == band) {
                if let Some(c) = s.cn0 {
                    all[k] = c;
                    k += 1;
                }
            }
            all[..k].sort_unstable_by(|a, b| b.cmp(a));
            let k = k.min(4);
            st.top4 = all[..k].iter().map(|&v| f32::from(v)).sum::<f32>() / k as f32;
        }
        st
    }
}

/// C/N0 statistics of one band of a [`SatTable`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cn0Stats {
    /// Entries listed (tracked or not).
    pub listed: u8,
    /// Entries with a C/N0.
    pub tracked: u8,
    /// Entries flagged not plausible.
    pub implausible: u8,
    /// Mean C/N0 of the tracked entries, dB-Hz.
    pub mean: f32,
    /// Variance of the C/N0 of the tracked entries, dB².
    pub var: f32,
    /// Mean of the (up to) four strongest, dB-Hz.
    pub top4: f32,
    /// Strongest, dB-Hz.
    pub max: u8,
    /// Weakest tracked, dB-Hz.
    pub min: u8,
    /// Tracked at or above [`crate::integrity::CN0_TOO_STRONG`].
    pub strong: u8,
}

/// Collects NMEA GSV entries of one epoch and publishes them at the epoch's end.
#[derive(Clone, Copy)]
pub(crate) struct Collector {
    pub(crate) done: SatTable,
    building: SatTable,
    /// A GSV arrived this epoch (even one listing no satellite).
    saw_gsv: bool,
}

impl Collector {
    pub(crate) const fn new() -> Self {
        Collector { done: SatTable::new(), building: SatTable::new(), saw_gsv: false }
    }

    /// The epoch ended (the fix was emitted): publish what GSV said, if a GSV came (an empty
    /// sky is news too: a receiver that lost everything).
    pub(crate) fn publish(&mut self) {
        if self.saw_gsv {
            self.saw_gsv = false;
            let seq = self.done.seq;
            self.done = self.building;
            self.done.seq = seq.wrapping_add(1).max(1);
            self.building.clear();
        }
    }

    /// A UBX-NAV-SAT payload: a whole epoch at once.
    pub(crate) fn nav_sat(&mut self, p: &[u8]) {
        self.done.set_from_nav_sat(p);
    }

    /// A GSV sentence (fields split on commas). Entries of a lost or partial group are
    /// still kept: each entry stands on its own.
    pub(crate) fn gsv(&mut self, f: &[&[u8]]) {
        if f.len() < 4 || f[0].len() < 2 {
            return;
        }
        let talker = &f[0][..2];
        let Some(talker_sys) = talker_system(talker) else { return };
        self.saw_gsv = true;
        let rest = f.len() - 4;
        let signal = if rest % 4 == 1 { num(f[f.len() - 1]).unwrap_or(0) } else { 0 };
        for i in 0..rest / 4 {
            let s = &f[4 + 4 * i..8 + 4 * i];
            let Some(prn) = num(s[0]) else { continue };
            let sys = if talker_sys == System::Gps && (193..=202).contains(&prn) {
                System::Qzss
            } else {
                talker_sys
            };
            let mut plausible = prn_ok(talker, prn);
            let elev = match signed(s[1]) {
                Field::Empty => None,
                Field::Bad => {
                    plausible = false;
                    None
                }
                Field::Val(e) => {
                    if !(-90..=90).contains(&e) {
                        plausible = false;
                        None
                    } else {
                        Some(e as i8)
                    }
                }
            };
            let azim = match signed(s[2]) {
                Field::Empty => None,
                Field::Bad => {
                    plausible = false;
                    None
                }
                Field::Val(a) => {
                    if !(0..=359).contains(&a) {
                        plausible = false;
                        None
                    } else {
                        Some(a as u16)
                    }
                }
            };
            let cn0 = num(s[3]).map(|c| c.min(99) as u8).filter(|&c| c > 0);
            self.building.push(Sat {
                sys,
                prn: prn.min(u32::from(u16::MAX)) as u16,
                band: Band::of(sys, signal),
                elev,
                azim,
                cn0,
                used: false,
                plausible,
            });
        }
    }
}

fn talker_system(t: &[u8]) -> Option<System> {
    Some(match t {
        b"GP" => System::Gps,
        b"GL" => System::Glonass,
        b"GA" => System::Galileo,
        b"GB" | b"BD" => System::Beidou,
        b"GQ" => System::Qzss,
        b"GI" => System::Navic,
        _ => return None,
    })
}

/// Is `prn` a possible NMEA satellite number for this talker? See the module table.
fn prn_ok(talker: &[u8], prn: u32) -> bool {
    match talker {
        b"GP" => matches!(prn, 1..=64 | 193..=202),
        b"GL" => matches!(prn, 65..=96),
        b"GA" => matches!(prn, 1..=36 | 301..=336),
        b"GB" | b"BD" => matches!(prn, 1..=63 | 201..=264 | 401..=437),
        b"GQ" => matches!(prn, 1..=10 | 193..=202),
        b"GI" => matches!(prn, 1..=14),
        _ => true,
    }
}

enum Field {
    Empty,
    Bad,
    Val(i32),
}

fn signed(f: &[u8]) -> Field {
    if f.is_empty() {
        return Field::Empty;
    }
    let (neg, d) = match f {
        [b'-', rest @ ..] => (true, rest),
        _ => (false, f),
    };
    match num(d) {
        Some(v) if v < 100_000 => Field::Val(if neg { -(v as i32) } else { v as i32 }),
        _ => Field::Bad,
    }
}

fn num(f: &[u8]) -> Option<u32> {
    if f.is_empty() || f.len() > 9 || !f.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(f.iter().fold(0u32, |a, &c| a * 10 + u32::from(c - b'0')))
}
