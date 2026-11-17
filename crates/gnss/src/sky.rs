//! Satellites in view and in use, per constellation and frequency band, from NMEA GSV and GSA.
//!
//! Talkers: `GP` GPS, `GL` GLONASS, `GA` Galileo, `GB`/`BD` BeiDou, `GQ` QZSS, `GI` NavIC,
//! `GN` combined. NMEA 4.10 adds a *signal ID* as the last GSV field (which band a group of
//! satellites was tracked on) and a *system ID* as the last GSA field (which constellation a
//! `GNGSA` line is about). Both are optional: older receivers and NMEA 4.0 output omit them,
//! and then GSV counts as the L1 band and a `GNGSA` line's constellation is read from the
//! satellite numbers.
//!
//! Signal IDs (NMEA 0183 4.10/4.11 table), grouped into two bands:
//!
//! | System | L1 band | L5 band |
//! |---|---|---|
//! | GPS (1) | 1 L1 C/A, 2 L1 P(Y), 3 L1 M | 7 L5 I, 8 L5 Q |
//! | GLONASS (2) | 1 G1 C/A, 2 G1 P | (none) |
//! | Galileo (3) | 6 E1 A, 7 E1 B/C | 1 E5a, 2 E5b, 3 E5 AltBOC |
//! | BeiDou (4) | 1 B1I, 2 B1Q, 3 B1C, 4 B1A | 5 B2a, 6 B2b, 7 B2a+b |
//! | QZSS (5) | 1 L1 C/A, 2 L1C (D), 3 L1C (P) | 7 L5 (D), 8 L5 (P) |
//!
//! Other signal IDs (L2, L6, B3, ...) are counted as `other`. Signal 0 means "all signals" and
//! counts as L1. The LC29H(AA) reports GPS L1 = 1, GPS L5 = 8, Galileo E1 = 7, E5a = 1,
//! BeiDou B1I = 1, B2a = 5, which fits this table. It also reports QZSS satellites (193-199)
//! under the `GP` talker and system ID 1; they are moved to QZSS by their number.

/// A GNSS constellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    /// GPS (and SBAS reported under it).
    Gps,
    /// GLONASS.
    Glonass,
    /// Galileo.
    Galileo,
    /// BeiDou.
    Beidou,
    /// QZSS.
    Qzss,
    /// NavIC (IRNSS).
    Navic,
}

impl System {
    /// All systems, in display order.
    pub const ALL: [System; 6] =
        [System::Gps, System::Glonass, System::Galileo, System::Beidou, System::Qzss, System::Navic];

    fn idx(self) -> usize {
        self as usize
    }

    /// NMEA 4.10 system ID.
    fn from_id(id: u32) -> Option<System> {
        Some(match id {
            1 => System::Gps,
            2 => System::Glonass,
            3 => System::Galileo,
            4 => System::Beidou,
            5 => System::Qzss,
            6 => System::Navic,
            _ => return None,
        })
    }

    fn from_talker(t: &[u8]) -> Option<System> {
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

    /// From a satellite number when nothing else says (NMEA 4.0 / u-blox extended numbering).
    fn from_prn(prn: u32) -> Option<System> {
        Some(match prn {
            1..=64 => System::Gps, // 33-64: SBAS
            65..=96 => System::Glonass,
            193..=202 => System::Qzss,
            203..=264 => System::Beidou,
            301..=336 => System::Galileo,
            401..=437 => System::Beidou,
            _ => return None,
        })
    }

    /// Short name for logs and JSON.
    pub fn name(self) -> &'static str {
        match self {
            System::Gps => "gps",
            System::Glonass => "glonass",
            System::Galileo => "galileo",
            System::Beidou => "beidou",
            System::Qzss => "qzss",
            System::Navic => "navic",
        }
    }
}

/// A frequency band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// L1 / E1 / B1 / G1.
    L1,
    /// L5 / E5 / B2.
    L5,
    /// Anything else (L2, L6, B3, ...).
    Other,
}

impl Band {
    pub(crate) fn of(sys: System, signal: u32) -> Band {
        match (sys, signal) {
            (_, 0) => Band::L1,
            (System::Gps, 1..=3) | (System::Qzss, 1..=3) => Band::L1,
            (System::Gps, 7 | 8) | (System::Qzss, 7 | 8) => Band::L5,
            (System::Glonass, 1 | 2) => Band::L1,
            (System::Galileo, 6 | 7) => Band::L1,
            (System::Galileo, 1..=3) => Band::L5,
            (System::Beidou, 1..=4) => Band::L1,
            (System::Beidou, 5..=7) => Band::L5,
            (System::Navic, 1) => Band::L5,
            _ => Band::Other,
        }
    }
}

/// Satellites of one constellation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConstellationView {
    /// Satellites in view (on any band).
    pub in_view: u8,
    /// Satellites with a signal (C/N0 reported) on the L1 band.
    pub l1: u8,
    /// Satellites with a signal on the L5 band. Non-zero means the receiver is dual-band.
    pub l5: u8,
    /// Satellites used in the solution (GSA).
    pub used: u8,
    /// Strongest C/N0 on any band, dB-Hz (0 if none).
    pub best_cn0: u8,
}

const SLOTS: usize = 16;
/// A group not refreshed for this many epochs is forgotten (a band switched off, a
/// constellation disabled).
const MAX_AGE: u32 = 3;

#[derive(Clone, Copy, Default)]
struct Counts {
    in_view: u8,
    tracked: u8,
    best_cn0: u8,
}

#[derive(Clone, Copy)]
struct Slot {
    sys: System,
    signal: u32,
    /// From the GSV header's "satellites in view" field (the last complete group).
    done: Counts,
    /// Being collected (messages 1..n of the current group).
    partial: Counts,
    next_msg: u8,
    epoch: u32,
    live: bool,
}

#[derive(Clone, Copy)]
struct Used {
    count: u8,
    epoch: u32,
}

/// The sky view: updated from every GSV and GSA sentence. Read it with
/// [`crate::Parser::sky`].
#[derive(Clone, Copy)]
pub struct SkyView {
    slots: [Slot; SLOTS],
    used: [Used; 6],
    /// Counts navigation epochs (one per fix the parser emits).
    epoch: u32,
    /// GSA fix mode of the newest GSA: 1 none, 2 2D, 3 3D.
    pub(crate) gsa_mode: Option<u8>,
    gsa_mode_epoch: u32,
    /// PDOP from the newest GSA.
    pub(crate) gsa_pdop: Option<f32>,
    /// A sentence carried a signal ID (NMEA 4.10+).
    pub signal_ids: bool,
}

impl Default for SkyView {
    fn default() -> Self {
        Self::new()
    }
}

impl SkyView {
    /// Nothing seen.
    pub const fn new() -> Self {
        const S: Slot = Slot {
            sys: System::Gps,
            signal: 0,
            done: Counts { in_view: 0, tracked: 0, best_cn0: 0 },
            partial: Counts { in_view: 0, tracked: 0, best_cn0: 0 },
            next_msg: 0,
            epoch: 0,
            live: false,
        };
        SkyView {
            slots: [S; SLOTS],
            used: [Used { count: 0, epoch: 0 }; 6],
            epoch: 1,
            gsa_mode: None,
            gsa_mode_epoch: 0,
            gsa_pdop: None,
            signal_ids: false,
        }
    }

    pub(crate) fn next_epoch(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }

    fn fresh(&self, e: u32) -> bool {
        e != 0 && self.epoch.wrapping_sub(e) <= MAX_AGE
    }

    /// The GSA fix mode if a GSA arrived recently.
    pub(crate) fn mode(&self) -> Option<u8> {
        if self.fresh(self.gsa_mode_epoch) { self.gsa_mode } else { None }
    }

    /// One constellation.
    pub fn constellation(&self, sys: System) -> ConstellationView {
        let mut v = ConstellationView::default();
        for s in self.slots.iter().filter(|s| s.live && s.sys == sys && self.fresh(s.epoch)) {
            v.in_view = v.in_view.max(s.done.in_view);
            v.best_cn0 = v.best_cn0.max(s.done.best_cn0);
            match Band::of(s.sys, s.signal) {
                Band::L1 => v.l1 = v.l1.saturating_add(s.done.tracked),
                Band::L5 => v.l5 = v.l5.saturating_add(s.done.tracked),
                Band::Other => {}
            }
        }
        let u = self.used[sys.idx()];
        if self.fresh(u.epoch) {
            v.used = u.count;
        }
        v
    }

    /// Satellites in view, all constellations.
    pub fn total_in_view(&self) -> u16 {
        System::ALL.iter().map(|&s| u16::from(self.constellation(s).in_view)).sum()
    }

    /// Satellites used, all constellations (from GSA).
    pub fn total_used(&self) -> u16 {
        System::ALL.iter().map(|&s| u16::from(self.constellation(s).used)).sum()
    }

    /// Any satellite tracked on the L5 band right now.
    pub fn dual_band(&self) -> bool {
        System::ALL.iter().any(|&s| self.constellation(s).l5 > 0)
    }

    /// A GSV sentence (fields split on commas, `f[0]` = `xxGSV`).
    pub(crate) fn gsv(&mut self, f: &[&[u8]]) {
        if f.len() < 4 {
            return;
        }
        let Some(talker_sys) = System::from_talker(&f[0][..2]) else { return };
        let (Some(total), Some(msg), Some(_in_view)) = (uint(f[1]), uint(f[2]), uint(f[3])) else { return };
        if total == 0 || msg == 0 || msg > total {
            return;
        }
        // Header, then 4 fields per satellite, then an optional signal ID.
        let rest = f.len() - 4;
        let signal = if rest % 4 == 1 {
            self.signal_ids = true;
            uint(f[f.len() - 1]).unwrap_or(0)
        } else {
            0
        };
        let nsat = rest / 4;
        // QZSS under the GP talker (LC29H(AA)): split by satellite number.
        let mut by_sys: [(Counts, bool); 2] = [(Counts::default(), false); 2];
        for i in 0..nsat {
            let s = &f[4 + 4 * i..8 + 4 * i];
            let Some(prn) = uint(s[0]) else { continue };
            let sys = if talker_sys == System::Gps && matches!(prn, 193..=202) { 1 } else { 0 };
            let c = &mut by_sys[sys].0;
            by_sys[sys].1 = true;
            c.in_view = c.in_view.saturating_add(1);
            if let Some(cn0) = uint(s[3]) {
                c.tracked = c.tracked.saturating_add(1);
                c.best_cn0 = c.best_cn0.max(cn0.min(99) as u8);
            }
        }
        for (k, (c, any)) in by_sys.iter().enumerate() {
            let sys = if k == 1 { System::Qzss } else { talker_sys };
            // Under GP every message is also part of a QZSS group (often an empty one).
            if k == 1 && talker_sys != System::Gps {
                continue;
            }
            let _ = any;
            let epoch = self.epoch;
            let Some(slot) = self.slot(sys, signal) else { return };
            if msg == 1 {
                slot.partial = Counts::default();
                slot.next_msg = 1;
            }
            if slot.next_msg != msg as u8 {
                // A message was lost: wait for the next group.
                slot.next_msg = 0;
                continue;
            }
            slot.partial.in_view = slot.partial.in_view.saturating_add(c.in_view);
            slot.partial.tracked = slot.partial.tracked.saturating_add(c.tracked);
            slot.partial.best_cn0 = slot.partial.best_cn0.max(c.best_cn0);
            slot.next_msg += 1;
            if msg == total {
                // Counted from the entries, not the header's total: on the LC29H(AA) the GP
                // header counts QZSS satellites too.
                slot.done = slot.partial;
                slot.epoch = epoch;
                slot.next_msg = 0;
            }
        }
    }

    fn slot(&mut self, sys: System, signal: u32) -> Option<&mut Slot> {
        let epoch = self.epoch;
        let i = match self.slots.iter().position(|s| s.live && s.sys == sys && s.signal == signal) {
            Some(i) => i,
            None => {
                // A free slot, else the oldest.
                let i = self.slots.iter().position(|s| !s.live).unwrap_or_else(|| {
                    (0..SLOTS).max_by_key(|&i| epoch.wrapping_sub(self.slots[i].epoch)).unwrap_or(0)
                });
                self.slots[i] = Slot {
                    sys,
                    signal,
                    done: Counts::default(),
                    partial: Counts::default(),
                    next_msg: 0,
                    epoch: 0,
                    live: true,
                };
                i
            }
        };
        Some(&mut self.slots[i])
    }

    /// A GSA sentence. Several per epoch (one per constellation) on multi-GNSS receivers.
    pub(crate) fn gsa(&mut self, f: &[&[u8]]) {
        if f.len() < 18 {
            return;
        }
        let mode = uint(f[2]);
        let prns: [Option<u32>; 12] = core::array::from_fn(|i| uint(f[3 + i]));
        let sys = if f.len() >= 19 {
            self.signal_ids = true;
            uint(f[18]).and_then(System::from_id)
        } else {
            None
        }
        .or_else(|| System::from_talker(&f[0][..2]))
        .or_else(|| prns.iter().flatten().find_map(|&p| System::from_prn(p)));
        if let Some(m) = mode {
            self.gsa_mode = Some(m.min(9) as u8);
            self.gsa_mode_epoch = self.epoch;
        }
        if let Some(p) = dec(f[15]) {
            self.gsa_pdop = Some(p);
        }
        let epoch = self.epoch;
        let mut count = [0u8; 6];
        for &p in prns.iter().flatten() {
            // QZSS reported with system ID 1 (LC29H(AA)) or under GP: by its number.
            let s = match (sys, System::from_prn(p)) {
                (_, Some(System::Qzss)) => System::Qzss,
                (Some(s), _) => s,
                (None, Some(s)) => s,
                (None, None) => continue,
            };
            count[s.idx()] = count[s.idx()].saturating_add(1);
        }
        // A line names one constellation; it replaces that constellation's count (and the
        // QZSS count when QZSS satellites ride along on a GPS line).
        if let Some(s) = sys {
            for t in [s, System::Qzss] {
                let u = &mut self.used[t.idx()];
                if t == s || count[t.idx()] > 0 {
                    // Several GSA lines for one system in one epoch (NMEA 4.0 splits after
                    // 12): add to this epoch's count instead of replacing it.
                    let base = if u.epoch == epoch && t == s { u.count } else { 0 };
                    u.count = base.saturating_add(count[t.idx()]);
                    u.epoch = epoch;
                }
            }
        }
    }
}

fn uint(f: &[u8]) -> Option<u32> {
    if f.is_empty() || f.len() > 9 || !f.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(f.iter().fold(0u32, |a, &c| a * 10 + u32::from(c - b'0')))
}

fn dec(f: &[u8]) -> Option<f32> {
    let s = core::str::from_utf8(f).ok()?;
    s.parse::<f32>().ok()
}
